#!/bin/bash

set -euo pipefail

# Rollback, ignore error in last log entry (assumption: no bit-rot / hardware failure / disk corruption)
tentris rollback --use-latest-auto-snapshot --replay --ignore-error-in-last-log-entry
tentris serve & disown

sleep 2
