# Test oracle: a stock Naemon installed from the project's binary packages.
# It is used only as a black box. Nothing from it is copied into this repo.
#
# The package repository keeps only the latest release, so a rebuild fails
# once a newer Naemon is published. Raise the versions deliberately then.
FROM debian:13-slim

ARG NAEMON_CORE_VERSION=1.5.2-1
ARG NAEMON_LIVESTATUS_VERSION=1.5.1-1

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && curl -fsS -o /etc/apt/trusted.gpg.d/naemon.asc \
      "https://build.opensuse.org/projects/home:naemon/signing_keys/download?kind=gpg" \
 && echo "deb [signed-by=/etc/apt/trusted.gpg.d/naemon.asc] http://download.opensuse.org/repositories/home:/naemon/Debian_13/ ./" \
      > /etc/apt/sources.list.d/naemon-stable.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      "naemon-core=${NAEMON_CORE_VERSION}" "naemon-livestatus=${NAEMON_LIVESTATUS_VERSION}" \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir /case && chmod 1777 /case

COPY entry.sh /usr/local/bin/oracle-entry
ENTRYPOINT ["/usr/local/bin/oracle-entry"]
