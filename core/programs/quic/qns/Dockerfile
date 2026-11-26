# The Hirsel QUIC endpoint for the QUIC Interop Runner (law Q5). run.py stages this file
# with the built endpoint binary and run_endpoint.sh.
FROM martenseemann/quic-network-simulator-endpoint:latest
RUN apt-get update && apt-get install -y --no-install-recommends openssl && rm -rf /var/lib/apt/lists/*
COPY endpoint /endpoint
COPY run_endpoint.sh /run_endpoint.sh
RUN chmod +x /run_endpoint.sh /endpoint && mkdir -p /hq && chmod 1777 /hq
ENTRYPOINT ["/run_endpoint.sh"]
