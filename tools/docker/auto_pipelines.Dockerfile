FROM cuelang/cue AS cue

FROM debian:bullseye-slim AS build_gitlab_pipelines
    COPY --from=cue /usr/bin/cue /usr/bin/cue
    WORKDIR /output
    WORKDIR /build
    COPY targets.json .
    COPY tools/docker/gitlab_tool.cue .
    RUN cue output ./... > /output/gitlab_pipelines.yml

FROM scratch AS gitlab_pipelines
    COPY --from=build_gitlab_pipelines /output/ /
