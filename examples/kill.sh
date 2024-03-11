#!/bin/bash

set -euo pipefail

# Kill all tentris instances (must not have other tentris instances running)
killall -s SIGKILL tentris
