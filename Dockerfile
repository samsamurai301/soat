# The SOAT server image. Built by scripts/release.sh from a binary it
# compiled in Debian 12 (docs/releases.md); the image adds no compiler.
FROM debian:bookworm-slim AS dirs
RUN mkdir -p /var/lib/soat/storage

FROM gcr.io/distroless/cc-debian12:nonroot
ARG BINARY=target/release-x86_64-unknown-linux-gnu/release/soat
COPY ${BINARY} /usr/local/bin/soat
# Uploads go here unless S3_BUCKET is set: mount a volume on it.
COPY --from=dirs --chown=nonroot:nonroot /var/lib/soat /var/lib/soat
ENV STORAGE_DIR=/var/lib/soat/storage PORT=8080
WORKDIR /var/lib/soat
USER nonroot
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/soat"]
