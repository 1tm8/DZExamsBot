# ─── مرحلة البناء ───────────────────────────────────────────────
FROM rust:1-bookworm AS builder
WORKDIR /app

# 1) بناء التبعيات وحدها في طبقة مخزّنة (تُعاد الاستفادة منها عند تغيّر الكود فقط)
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/bin \
    && printf 'fn main() {}\n' > src/main.rs \
    && printf 'fn main() {}\n' > src/bin/crawler.rs \
    && printf '' > src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

# 2) الكود الحقيقي
COPY src ./src
RUN touch src/main.rs src/lib.rs src/bin/crawler.rs \
    && cargo build --release --locked

# ─── مرحلة التشغيل ──────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# شهادات TLS فقط — لا حاجة لـ OpenSSL (rustls) ولا لـ Python
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1000 botuser

WORKDIR /app
COPY --from=builder /app/target/release/dzexams-bot /usr/local/bin/dzexams-bot
COPY --from=builder /app/target/release/dzexams-crawler /usr/local/bin/dzexams-crawler
RUN mkdir -p /app/data/tmp && chown -R botuser:botuser /app
USER botuser

ENV DB_PATH=/app/data/dzexams.db \
    TMP_DIR=/app/data/tmp

HEALTHCHECK --interval=60s --timeout=10s --start-period=30s --retries=3 \
  CMD ["dzexams-bot", "--healthcheck"]

CMD ["dzexams-bot"]
