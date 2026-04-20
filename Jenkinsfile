/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2021 Joyent, Inc.
 * Copyright 2025 MNX Cloud, Inc.
 * Copyright 2026 Edgecast Cloud LLC.
 */

/*
 * Monorepo Jenkinsfile: builds both the triton-api zone image and the
 * tritonadm GZ tool tarball from the same commit. Modeled on the
 * manta-rebalancer branch's root Jenkinsfile but with one
 * joyBuildImageAndUpload stage per image subdir. Splitting into
 * per-image Jenkinsfiles (as suggested in docs/design/zone-image-builds.md)
 * is a future refactor once the pipeline is validated end-to-end.
 */

@Library('jenkins-joylib@v1.0.9') _

pipeline {

    agent {
        label joyCommonLabels(image_ver: '24.4.1', pi: '20210826T002459Z')
    }

    options {
        buildDiscarder(logRotator(numToKeepStr: '30'))
        timestamps()
    }

    parameters {
        string(
            name: 'AGENT_PREBUILT_AGENT_BRANCH',
            defaultValue: '',
            description: 'The branch to use for the agents ' +
                'that are included in this component.<br/>' +
                'With an empty value, the build will look for ' +
                'agents from the same branch name as the ' +
                'component, before falling back to "master".'
        )
    }

    environment {
        /* Help rustup find certs */
        SSL_CERT_FILE = '/etc/ssl/certs/ca-certificates.crt'
    }

    stages {
        /*stage('check') {
            steps{
                sh('make check')
            }
        }*/
        stage('build triton-api image') {
           steps {
               joyBuildImageAndUpload(dir: 'images/triton-api')
           }
        }
        stage('build tritonadm tarball') {
           steps {
               joyBuildImageAndUpload(dir: 'images/tritonadm')
           }
        }
    }

    post {
        always {
            joySlackNotifications()
        }
    }
}
