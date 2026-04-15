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
        /* attempt to tell rustup where our certs are */
        SSL_CERT_FILE = '/etc/ssl/certs/ca-certificates.crt'
    }

    stages {
        stage('check') {
            steps{
                sh('make check')
            }
        }
        stage('build image and upload') {
           steps {
               joyBuildImageAndUpload(dir: 'images/rebalancer')
           }
        }
        stage('mako') {
            // This only works for master branches. For development builds
            // of the rebalancer, the developer should trigger a development
            // branch build of mako with AGENT_PREBUILT_AGENT_BRANCH pointing
            // at the corresponding development branch of the rebalancer.
            when {
                branch 'master'
            }
            steps {
                build(job:'TritonDataCenter/manta-mako/master', wait: false)
            }
        }
    }

    post {
        always {
            joySlackNotifications()
        }
    }
}
