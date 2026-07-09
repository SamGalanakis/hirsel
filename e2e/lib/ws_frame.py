#!/usr/bin/env python3
import base64
import json
import os
import socket
import struct
import sys
import time


def read_exact(sock, count):
    data = b""
    while len(data) < count:
        chunk = sock.recv(count - len(data))
        if not chunk:
            raise RuntimeError("socket closed")
        data += chunk
    return data


def encode_frame(text):
    payload = text.encode("utf-8")
    mask = os.urandom(4)
    first = b"\x81"
    length = len(payload)
    if length < 126:
        header = first + bytes([0x80 | length])
    elif length < 65536:
        header = first + bytes([0x80 | 126]) + struct.pack("!H", length)
    else:
        header = first + bytes([0x80 | 127]) + struct.pack("!Q", length)
    masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
    return header + mask + masked


def read_frame(sock):
    first, second = read_exact(sock, 2)
    opcode = first & 0x0F
    masked = bool(second & 0x80)
    length = second & 0x7F
    if length == 126:
        length = struct.unpack("!H", read_exact(sock, 2))[0]
    elif length == 127:
        length = struct.unpack("!Q", read_exact(sock, 8))[0]
    mask = read_exact(sock, 4) if masked else b""
    payload = read_exact(sock, length)
    if masked:
        payload = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
    if opcode == 8:
        raise RuntimeError("websocket closed")
    if opcode == 9:
        return None
    if opcode != 1:
        return None
    return payload.decode("utf-8")


def main():
    if len(sys.argv) < 5:
        raise SystemExit("usage: ws_frame.py HOST PORT TOKEN JSON_FRAME [WAIT_TYPE]")
    host = sys.argv[1]
    port = int(sys.argv[2])
    token = sys.argv[3]
    frame = sys.argv[4]
    wait_type = sys.argv[5] if len(sys.argv) > 5 else None

    key = base64.b64encode(os.urandom(16)).decode("ascii")
    sock = socket.create_connection((host, port), timeout=10)
    sock.settimeout(10)
    request = (
        f"GET /ws HTTP/1.1\r\n"
        f"Host: {host}:{port}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "\r\n"
    )
    sock.sendall(request.encode("ascii"))
    response = b""
    while b"\r\n\r\n" not in response:
        response += sock.recv(4096)
    if b" 101 " not in response.split(b"\r\n", 1)[0]:
        raise RuntimeError(response.decode("utf-8", "replace"))

    hello = {"type": "hello", "token": token, "last_seen_msg_id": None}
    sock.sendall(encode_frame(json.dumps(hello)))
    deadline = time.time() + 10
    while time.time() < deadline:
        message = read_frame(sock)
        if message is None:
            continue
        print(message)
        if json.loads(message).get("type") == "hello_ok":
            break
    else:
        raise RuntimeError("hello_ok not received")

    sock.sendall(encode_frame(frame))
    if not wait_type:
        return
    deadline = time.time() + 10
    while time.time() < deadline:
        message = read_frame(sock)
        if message is None:
            continue
        print(message)
        if json.loads(message).get("type") == wait_type:
            return
    raise RuntimeError(f"{wait_type} not received")


if __name__ == "__main__":
    main()
