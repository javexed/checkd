# Wine, to run checkd-agent's Windows build in runs/agent-windows/run.
FROM debian:trixie
RUN apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends wine wine64 && rm -rf /var/lib/apt/lists/*
ENV WINEDEBUG=-all HOME=/tmp
RUN wineboot -i >/dev/null 2>&1 || true
