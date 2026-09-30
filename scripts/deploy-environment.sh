#!/usr/bin/env bash
set -euo pipefail

mode="${1:?usage: deploy-environment.sh deploy|rollback}"
case "$mode" in
  deploy)
    command="${DEPLOY_COMMAND:-}"
    ;;
  rollback)
    command="${ROLLBACK_COMMAND:-}"
    ;;
  *)
    echo "unsupported deployment operation: $mode" >&2
    exit 2
    ;;
esac

if [[ -z "$command" ]]; then
  echo "${mode^^}_COMMAND must be configured in the GitHub environment secrets." >&2
  exit 1
fi
if [[ -z "${IMAGE:-}" ]]; then
  echo "IMAGE must be set to the container image being promoted." >&2
  exit 1
fi

export IMAGE
bash -euo pipefail -c "$command"
