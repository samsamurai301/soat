# Releases

Releases are built on a maintainer's machine, not in GitHub Actions:
`scripts/release.sh` makes the binaries, their checksums and the
container image.

```sh
scripts/release.sh                  # binaries and image
scripts/release.sh --no-image       # binaries only
ARM64=1 scripts/release.sh          # also Linux arm64 (emulated, slow)
EXTRA_CA=/path/ca.pem scripts/release.sh   # behind a TLS-inspecting proxy
```

It refuses to run with uncommitted changes, so a release is exactly its
commit. The version comes from `Cargo.toml` (`[workspace.package]`).
Everything lands in `dist/<version>/`:

| File | What |
|---|---|
| `soat-<version>-x86_64-unknown-linux-gnu.tar.gz` | Linux x86-64: `soat`, `LICENSE`, `README.md` |
| `soat-<version>-aarch64-unknown-linux-gnu.tar.gz` | Linux arm64, with `ARM64=1` |
| `soat-<version>-aarch64-apple-darwin.tar.gz`, `-x86_64-apple-darwin` | macOS, when run on a Mac |
| `SHA256SUMS` | Checksums of the archives (`sha256sum -c SHA256SUMS`) |
| `soat-<version>-image-amd64.tar.gz` | The image, for `docker load` |

The image is also tagged `soat:<version>` and `soat:latest` locally.

## How it builds

- **Linux** builds in Docker, in `rust:1.100-bookworm`. So the binary needs
  glibc 2.36 or newer (Debian 12, Ubuntu 22.04, RHEL 9 and newer), not the
  build machine's. Cargo's downloads are cached in the `soat-cargo-registry`
  and `soat-cargo-git` volumes. A first build takes about 7 minutes on 4
  cores.
- **macOS** builds natively for Apple silicon and Intel. It needs Xcode's
  command line tools.
- Without Docker on Linux, it builds natively, for this machine's glibc.

## The image

`Dockerfile` puts the Linux binary on `gcr.io/distroless/cc-debian12`:

- no shell and no package manager, about 150 MB;
- runs as an unprivileged user;
- uploads go to `/var/lib/soat/storage`, so mount a volume there (or set
  `S3_BUCKET`);
- listens on 8080.

```sh
docker load < dist/0.0.1/soat-0.0.1-image-amd64.tar.gz
docker run -p 8080:8080 \
  -e DATABASE_URL=postgres://soat:...@db/soat \
  -e SOAT_SECRET_KEY=... -e STORAGE_SECRET=... \
  -e SMTP_URL=smtps://user:pass@smtp.example.com -e MAIL_FROM=shop@example.com \
  -v soat-storage:/var/lib/soat/storage \
  soat:0.0.1
```

Kubernetes probes: `/health/live` and `/health/ready`
(`docs/operations.md`). Before upgrading a running install, read
`docs/backups.md`.

## Publishing

Publishing is a manual step with no Actions minutes: upload the files with
`gh release create v<version> dist/<version>/*`, and push the image to a
registry with `docker tag` and `docker push`.
