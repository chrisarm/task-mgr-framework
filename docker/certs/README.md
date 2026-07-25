# docker/certs — host-specific CA trust for image builds

Drop extra root CA certificates (PEM, `.crt` extension) here and they are
installed into the trust store of both recall-stack images at build time —
**before** any model download runs.

Why this exists: on hosts behind a TLS-inspecting proxy (e.g. Cloudflare
WARP / Zero Trust gateway), build-time downloads (Hugging Face GGUF,
`ollama pull`) fail certificate verification because the build container
does not trust the proxy's CA even though the host does. Copy the proxy CA
here (on Arch: extract it from `/etc/ssl/certs/ca-certificates.crt`, since
`/etc/ca-certificates/trust-source/anchors/` is root-only) and rebuild.

Certs are machine-specific and **gitignored** (`docker/certs/*.crt`); an
empty directory is valid — the build then installs nothing.
