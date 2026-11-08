"""Finite open-loop client for hello-web's one-response-per-connection workload."""
import concurrent.futures
import socket
import time

REQUEST = b'GET / HTTP/1.1\r\nHost: localhost\r\n\r\n'


def one(port, scheduled):
    time.sleep(max(0, scheduled - time.monotonic()))
    while True:
        try:
            connection = socket.create_connection(('127.0.0.1', port))
            break
        except ConnectionRefusedError:
            time.sleep(.001)
    with connection:
        connection.sendall(REQUEST)
        response = b''
        while True:
            data = connection.recv(4096)
            if not data:
                break
            response += data
        assert b' 200 ' in response and response.endswith(b'Hello, world'), response
    return (time.monotonic() - scheduled) * 1e6


def run(port, rate=2000):
    # Opening a readiness-probe connection would consume one server accept.
    with concurrent.futures.ThreadPoolExecutor(max_workers=32) as pool:
        start = time.monotonic()
        futures = [pool.submit(one, port, start + i / rate) for i in range(1024)]
        return [future.result() for future in futures]
