<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Zone Image Builds for a Rust Monorepo

## Background

This repo is a Rust monorepo that will ship multiple services as Triton zone
images. The existing eng.git build infrastructure (`Makefile.targ`,
`buildimage`, `bits-upload`) assumes one repo produces one image: a single
`NAME`, a single `RELEASE_TARBALL`, a single `buildimage` invocation. We need
an approach that lets each service define its own image while sharing the
monorepo's Cargo workspace and eng Makefile machinery.

### What ships and what doesn't

Not every service in the monorepo becomes a zone image. Developer tools like
`bugview-service` and `jira-stub-server` are run locally or in dev
environments, not deployed as Triton core services. Only services under
`images/` are intended for zone image builds.

### Reference repos

These repos contain the build infrastructure and reference implementations
used by this design:

- **[eng](https://github.com/TritonDataCenter/eng)** — shared Makefile
  infrastructure (`buildimage`, `bits-upload`, `Makefile.rust.*`,
  `Makefile.smf.*`). Consumed as `deps/eng` submodule.
- **[triton-origin-image](https://github.com/TritonDataCenter/triton-origin-image)** —
  base images that zone images are built on top of. We target
  `triton-origin-x86_64-24.4.1`.
- **[jenkins-joylib](https://github.com/TritonDataCenter/jenkins-joylib)** —
  shared Jenkins pipeline library (`joyBuildImageAndUpload`,
  `joyCommonLabels`, `joySlackNotifications`). Needs a small change for
  monorepo support (see below).
- **[triton-moirai](https://github.com/TritonDataCenter/triton-moirai)** —
  canonical example of a single-service Rust repo that builds a zone image.
  Our per-service Makefile and release target are modeled on this.

### Reference: how a single-service Rust repo does it

triton-moirai (cloud-load-balancer) is the simplest working example:

```
triton-moirai/
    Makefile          # NAME=cloud-load-balancer, ENGBLD_USE_BUILDIMAGE=true,
                      #   release target stages tarball, includes eng Makefiles
    Jenkinsfile       # calls joyBuildImageAndUpload()
    boot/setup.sh     # first-boot script
    smf/manifests/    # SMF service manifests
    src/              # Rust source
    deps/eng/         # eng submodule
```

The Makefile defines `NAME`, `BASE_IMAGE_UUID`, `RELEASE_TARBALL`,
`BUILDIMAGE_NAME`, `BUILDIMAGE_DESC`, and `BUILDIMAGE_PKGSRC`. The `release`
target builds the Rust binary, stages everything into a tarball with this
layout:

```
root/opt/triton/boot/setup.sh
root/opt/triton/<svc>/<binary>
root/opt/custom/smf/manifests/*.xml
site/.do-not-delete-me
```

Jenkins calls `make all release publish buildimage bits-upload`, which:
1. Compiles the binary (`all`)
2. Stages the tarball (`release`)
3. Copies it to `bits/` (`publish`)
4. Creates a ZFS image from origin image + tarball (`buildimage`)
5. Uploads the image to the image server (`bits-upload`)

### The monorepo problem

Our top-level Makefile cannot set a single `NAME` because the repo produces
multiple images. We need per-service values for `NAME`, `BASE_IMAGE_UUID`,
`RELEASE_TARBALL`, `BUILDIMAGE_*`, and per-service `release` targets that each
stage their own tarball.

## Design

### Per-service image directories

Each service that ships as a zone image gets a directory under `images/`:

```
monitor-reef/
    Makefile                        # existing: workspace dev commands
    Cargo.toml                      # existing: workspace definition
    deps/eng/                       # existing: eng submodule
    services/                       # existing: Rust service source
    images/
        image.defs.mk               # shared defaults for all images
        <service-name>/
            Makefile                 # per-image: NAME, BUILDIMAGE_*, release target
            Jenkinsfile              # per-image: Jenkins pipeline
            boot/
                setup.sh             # first-boot script
            smf/
                manifests/
                    postboot.xml     # transient: runs setup.sh on first boot
                    <service>.xml    # long-running service manifest
            sapi_manifests/
                <service>/
                    manifest.json    # config-agent: output path + post_cmd
                    template         # Mustache template for config generation
```

### Shared image definitions: `images/image.defs.mk`

Common variables and patterns shared by all images. Each per-service Makefile
includes this before the eng Makefiles:

```makefile
# Path back to repo root from images/<service>/
ENGBLD_REPO_ROOT := $(shell git rev-parse --show-toplevel)

# All images use the same eng submodule
ENGBLD_REQUIRE := $(shell git submodule update --init $(ENGBLD_REPO_ROOT)/deps/eng)

# Default origin image (services can override)
# triton-origin-x86_64-24.4.1
BASE_IMAGE_UUID ?= 41bd4100-eb86-409a-85b0-e649aadf6f62

# Common buildimage settings
ENGBLD_USE_BUILDIMAGE = true
# Build machine platform (matches jenkins-joylib label for 21.4.0 builders)
BUILD_PLATFORM = 20210826T002459Z

# Use the local copy of buildimage from deps/eng (needed for 24.4.1 origin)
ENGBLD_FORCE_LOCAL_BUILDIMAGE = true
```

### Per-service Makefile

Each `images/<service>/Makefile` sets service-specific variables and defines
the `release` target that stages the tarball. Example skeleton:

```makefile
NAME = my-service
DIR_NAME = my-svc

include ../image.defs.mk
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.defs
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.rust.defs
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.smf.defs

ROOT            := $(shell pwd)
RELEASE_TARBALL := $(NAME)-pkg-$(STAMP).tar.gz
RELSTAGEDIR     := $(ROOT)/proto

SMF_MANIFESTS = smf/manifests/postboot.xml smf/manifests/my-service.xml

# Image metadata
BUILDIMAGE_NAME = $(NAME)
BUILDIMAGE_DESC = Description of this Triton service
BUILDIMAGE_PKGSRC =

# Binaries to include (Cargo package names)
SERVICE_BINS = my-service

CLEAN_FILES += bits proto target

.PHONY: release_build
release_build:
	cd $(ENGBLD_REPO_ROOT) && $(CARGO) build --release -p $(SERVICE_BINS)

.PHONY: all
all: release_build

.PHONY: release
release: all
	@echo "Building $(RELEASE_TARBALL)"
	@rm -rf $(RELSTAGEDIR)
	@mkdir -p $(RELSTAGEDIR)/root/opt/triton/boot
	@mkdir -p $(RELSTAGEDIR)/root/opt/triton/$(DIR_NAME)
	@mkdir -p $(RELSTAGEDIR)/root/opt/custom/smf
	@mkdir -p $(RELSTAGEDIR)/root/opt/smartdc/$(DIR_NAME)/sapi_manifests
	@mkdir -p $(RELSTAGEDIR)/site
	@touch $(RELSTAGEDIR)/site/.do-not-delete-me
	cp $(ENGBLD_REPO_ROOT)/target/release/$(SERVICE_BINS) \
	    $(RELSTAGEDIR)/root/opt/triton/$(DIR_NAME)/
	cp -PR boot/* $(RELSTAGEDIR)/root/opt/triton/boot/
	cp -PR smf/* $(RELSTAGEDIR)/root/opt/custom/smf/
	cp -PR sapi_manifests/* \
	    $(RELSTAGEDIR)/root/opt/smartdc/$(DIR_NAME)/sapi_manifests/
	(cd $(RELSTAGEDIR) && $(TAR) -I pigz -cf $(ROOT)/$(RELEASE_TARBALL) root site)

.PHONY: publish
publish: release
	mkdir -p $(ENGBLD_BITS_DIR)/$(NAME)
	cp $(ROOT)/$(RELEASE_TARBALL) $(ENGBLD_BITS_DIR)/$(NAME)/$(RELEASE_TARBALL)

include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.deps
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.rust.targ
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.smf.targ
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.targ
```

### jenkins-joylib change: `dir` parameter for `joyBuildImageAndUpload`

The `joyBuildImageAndUpload()` shared library function needs a small change to
support monorepos. Currently it runs `make` from the workspace root with no
way to target a subdirectory:

```groovy
// BEFORE (vars/joyBuildImageAndUpload.groovy)
void call() {
    sh('''
set -o errexit
set -o pipefail

export ENGBLD_BITS_UPLOAD_IMGAPI=true
make print-BRANCH print-STAMP all release publish buildimage bits-upload''')
}
```

The change adds an optional `dir` parameter that uses `make -C` to run from a
subdirectory. Existing callers are unaffected since the parameter defaults to
`'.'`:

```groovy
// AFTER (vars/joyBuildImageAndUpload.groovy)
void call(Map args = [:]) {
    String makeDir = args.dir ?: '.';
    sh("""
set -o errexit
set -o pipefail

export ENGBLD_BITS_UPLOAD_IMGAPI=true
make -C ${makeDir} print-BRANCH print-STAMP all release publish buildimage bits-upload""")
}
```

Monorepo callers use: `joyBuildImageAndUpload(dir: 'images/my-service')`

### Per-service Jenkinsfile

Each image directory gets its own Jenkinsfile. Jenkins jobs are configured to
point at the repo with the Jenkinsfile path set to `images/<service>/Jenkinsfile`.

`joyBuildImageAndUpload()` in jenkins-joylib accepts an optional `dir`
parameter that runs make from a subdirectory (via `make -C`). This was added
to support monorepos without breaking existing single-service callers.

```groovy
@Library('jenkins-joylib@v1.0.8') _

pipeline {
    agent {
        label joyCommonLabels(image_ver: '21.4.0', pi: '20210826T002459Z')
    }
    options {
        buildDiscarder(logRotator(numToKeepStr: '30'))
        timestamps()
    }
    stages {
        stage('check') {
            steps {
                sh('make -C images/<service> check')
            }
        }
        stage('re-clean') {
            steps {
                sh('git clean -fdx')
            }
        }
        stage('build image and upload') {
            steps {
                joyBuildImageAndUpload(dir: 'images/<service>')
            }
        }
    }
    post {
        always {
            joySlackNotifications()
        }
    }
}
```

### SAPI integration

Each service includes a `sapi_manifests/<name>/` directory with:

**`manifest.json`**: tells config-agent where to write the config and what to
do afterward.

```json
{
    "name": "<service>",
    "path": "/opt/triton/<dir_name>/etc/config.json",
    "post_cmd": "/usr/sbin/svcadm restart <service>"
}
```

**`template`**: Mustache template that config-agent renders with SAPI metadata.

```json
{
    "datacenter_name": "{{{datacenter_name}}}",
    "instance_uuid": "{{auto.ZONENAME}}",
    "server_uuid": "{{auto.SERVER_UUID}}",
    "admin_ip": "{{auto.ADMIN_IP}}"
}
```

The `release` target copies `sapi_manifests/` into the tarball at
`root/opt/smartdc/<dir_name>/sapi_manifests/`. This is the conventional
location where config-agent looks for manifests.

### Boot and SMF

Each image ships at least two SMF manifests:

1. **`postboot.xml`**: a transient service (`site/postboot`) that runs
   `boot/setup.sh` once on first boot. This handles zone-specific setup:
   importing other SMF manifests, creating directories, initial config-agent
   run, etc.

2. **`<service>.xml`**: the long-running service manifest. Depends on
   `site/postboot` and network services. Exec method runs the Rust binary.

The `boot/setup.sh` script should:
- Guard against re-running (`/var/tmp/.first-boot-done`)
- Source smf_include.sh for exit codes
- Import the service SMF manifest
- Perform any one-time setup
- Touch the first-boot marker

### Top-level convenience targets

The repo-root Makefile can optionally provide convenience targets to build
images without `cd`-ing:

```makefile
# Image build targets (convenience wrappers)
image-%:
	$(MAKE) -C images/$* all release publish

image-%-buildimage:
	$(MAKE) -C images/$* buildimage

images-list:
	@ls -1 images/ | grep -v '\.mk$$'
```

## Decisions

1. **Origin image version**: New services target `triton-origin-x86_64-24.4.1`
   (UUID `41bd4100-eb86-409a-85b0-e649aadf6f62`). Individual services can
   override `BASE_IMAGE_UUID` if they need a different base.

2. **First service**: A dummy/test service will be built first on a SmartOS
   machine to validate the entire pipeline before shipping real services.

## Open questions

1. **Cargo build location**: The `release_build` target runs
   `cargo build --release` from the repo root. The resulting binary lands in
   `<repo>/target/release/`. This works but means all image builds share the
   same Cargo target directory — concurrent image builds of different services
   should be fine since Cargo handles locking internally.

2. **SAPI service registration**: New Triton services need to be registered in
   SAPI. This typically involves adding a service definition to
   `sdc-headnode/config/sapi/services/<name>/service.json`. The mechanics of
   this registration for new services should be documented once the first
   service is ready to deploy.

3. **eng Makefile compatibility**: The eng Makefiles use `$(TOP)` to refer to
   the project root. When running from `images/<service>/`, `$(TOP)` will
   resolve to that subdirectory, not the repo root. The `ENGBLD_REPO_ROOT`
   variable in `image.defs.mk` provides the actual repo root for Cargo
   commands and eng submodule paths. We need to verify that `ENGBLD_REPO_ROOT`
   is sufficient for all eng targets (`buildimage`, `bits-upload`, etc.), or
   whether `$(TOP)` also needs to be overridden.

4. **Jenkins pipeline strategy**: One Jenkins job per service image
   (recommended), or a single job that builds all images? Per-service jobs are
   simpler and allow independent build/deploy cycles.

## Prerequisites / TODO

- [ ] Commit and push the `joyBuildImageAndUpload` `dir` parameter change to
  jenkins-joylib
- [ ] Initialize `deps/eng` submodule in monitor-reef (`git submodule update --init deps/eng`)
- [ ] Build and test dummy service on SmartOS to validate the pipeline
- [ ] Resolve `$(TOP)` compatibility question with hands-on testing
