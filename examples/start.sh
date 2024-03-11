#!/bin/bash

set -euo pipefail

# Clean start
rm -rf tentris-data
tentris load < swdf.nt
tentris serve & disown

sleep 2
