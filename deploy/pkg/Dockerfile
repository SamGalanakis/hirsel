FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates nginx-core python3 && rm -rf /var/lib/apt/lists/*
COPY hirsel writer discovery /opt/registry/
COPY config/ /etc/hirsel/registry/
COPY nginx.conf /etc/nginx/nginx.conf
COPY start.py /opt/registry/start.py
ENTRYPOINT ["python3", "-u", "/opt/registry/start.py"]
