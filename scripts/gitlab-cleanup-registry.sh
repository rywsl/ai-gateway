#!/usr/bin/env sh
set -eu

: "${CI_API_V4_URL:?CI_API_V4_URL is required}"
: "${CI_PROJECT_ID:?CI_PROJECT_ID is required}"

TOKEN_HEADER="JOB-TOKEN: ${CI_JOB_TOKEN:-}"
if [ -n "${GITLAB_CLEANUP_TOKEN:-}" ]; then
  TOKEN_HEADER="PRIVATE-TOKEN: ${GITLAB_CLEANUP_TOKEN}"
fi

registry_host="${REGISTRY_HOST:-${CI_REGISTRY:?CI_REGISTRY is required}}"
registry_image="${GATEWAY_IMAGE:-${CI_REGISTRY_IMAGE:?CI_REGISTRY_IMAGE is required}}"
prefix="${registry_image#${registry_host}/}"
location_prefix="${registry_image}"
repositories_url="${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/registry/repositories?per_page=100"

echo "Looking up registry repositories for ${registry_image}"
repos_json="$(curl --silent --show-error --fail --header "${TOKEN_HEADER}" "${repositories_url}")"

repo_ids="$(printf '%s' "${repos_json}" | jq -r --arg prefix "${prefix}" --arg location_prefix "${location_prefix}" '
  .[]
  | select((.path == $prefix) or (.location == $location_prefix))
  | .id
')"

if [ -z "${repo_ids}" ]; then
  echo "No matching registry repositories found; skipping cleanup."
  exit 0
fi

for repo_id in ${repo_ids}; do
  echo "Deleting old SHA tags for registry repository ${repo_id}; keeping latest 5 SHA tags."
  curl --silent --show-error --fail --request DELETE \
    --header "${TOKEN_HEADER}" \
    --data-urlencode 'name_regex_delete=^[0-9a-f]{8,40}$' \
    --data-urlencode 'name_regex_keep=^latest$' \
    --data-urlencode 'keep_n=5' \
    "${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/registry/repositories/${repo_id}/tags"
done
