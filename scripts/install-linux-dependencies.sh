#!/bin/bash

set -o errexit
set -o nounset
set -o pipefail

apt update
apt install -y cmake libssl-dev pkg-config
