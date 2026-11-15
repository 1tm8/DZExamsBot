FROM python:3.12-slim
WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
# تشغيل بلا صلاحيات root: مستخدم مخصص يملك /app وبياناته
RUN useradd -m -u 1000 botuser \
    && mkdir -p /app/data /app/data/tmp \
    && chown -R botuser:botuser /app
COPY --chown=botuser:botuser . .
USER botuser
HEALTHCHECK --interval=60s --timeout=10s --start-period=30s \
  CMD python -c "import socket; socket.create_connection(('api.telegram.org', 443), 5)"
CMD ["python", "bot.py"]
