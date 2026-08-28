// SPDX-License-Identifier: Apache-2.0
import net from "node:net";
import { readFile, lstat, realpath } from "node:fs/promises";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
export class BridgeClient {
  constructor(home) { this.home = home; }
  async request(event, data, timeoutMs = 200) {
    const deadline = Date.now() + timeoutMs;
    this.home = await realpath(this.home);
    const privateFile = async name => {
      const path = join(this.home, "run", name), info = await lstat(path);
      if (!info.isFile() || info.isSymbolicLink() || (info.mode & 0o077) || info.size > 8192) throw Error("Unsafe Bridge runtime file");
      return readFile(path, "utf8");
    };
    const [description, token] = await Promise.all([privateFile("bridge-endpoint.json"), privateFile("bridge-token")]);
    const endpoint = JSON.parse(description), requestId = randomUUID();
    let options;
    if (endpoint.transport === "unix") {
      if (endpoint.path !== join(this.home, "run", "hardknock.sock")) throw Error("Unexpected Bridge socket");
      options = { path: endpoint.path };
    } else {
      const [host, port] = endpoint.address.split(":");
      if (host !== "127.0.0.1") throw Error("Bridge must be local");
      options = { host, port: Number(port) };
    }
    const payload = { event, ...(data === undefined ? {} : { data }) };
    const body = JSON.stringify({ protocol_version: "hardknock.bridge.v1", request_id: requestId, token, payload }) + "\n";
    if (Buffer.byteLength(body) > 1048576) throw Error("Bridge request too large");
    return new Promise((resolve, reject) => {
      const socket = net.createConnection(options), chunks = []; let size = 0, settled = false;
      const finish = (error, value) => {
        if (settled) return;
        settled = true; clearTimeout(timer); socket.destroy();
        error ? reject(error) : resolve(value);
      };
      const timer = setTimeout(() => finish(Error("Bridge deadline exceeded")), Math.max(1, deadline - Date.now()));
      socket.on("connect", () => socket.write(body));
      socket.on("error", error => finish(error));
      socket.on("end", () => finish(Error("Bridge disconnected")));
      socket.on("data", chunk => {
        size += chunk.length;
        if (size > 1048576) return finish(Error("Bridge response too large"));
        chunks.push(chunk);
        if (!chunk.includes(10)) return;
        try {
          const response = JSON.parse(Buffer.concat(chunks).toString("utf8").split("\n")[0]);
          if (response.protocol_version !== "hardknock.bridge.v1" || response.request_id !== requestId || !response.ok) throw Error("Bridge rejected event or response mismatch");
          finish(null, response.payload);
        } catch (error) { finish(error); }
      });
    });
  }
}
