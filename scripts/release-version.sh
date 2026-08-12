#!/usr/bin/env bash

release_version_is_valid() {
    local version=$1
    local core prerelease identifier
    local identifiers

    core=${version%%-*}
    if [[ ! "$core" =~ ^(0|[1-9][0-9]*)[.](0|[1-9][0-9]*)[.](0|[1-9][0-9]*)$ ]]; then
        return 1
    fi
    if [ "$version" = "$core" ]; then
        return 0
    fi

    prerelease=${version#"$core"-}
    if [[ ! "$prerelease" =~ ^[0-9A-Za-z-]+([.][0-9A-Za-z-]+)*$ ]]; then
        return 1
    fi

    IFS=. read -r -a identifiers <<<"$prerelease"
    for identifier in "${identifiers[@]}"; do
        if [[ "$identifier" =~ ^[0-9]+$ && "$identifier" != "0" && "$identifier" == 0* ]]; then
            return 1
        fi
    done
}

release_tag_is_valid() {
    local tag=$1
    [[ "$tag" == v* ]] && release_version_is_valid "${tag#v}"
}
