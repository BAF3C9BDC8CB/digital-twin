/**
 * Main application entry point.
 */

const http = require("http");
const url = require("url");

class AppServer {
    constructor(port) {
        this.port = port;
        this.routes = {};
    }

    get(path, handler) {
        this.routes[path] = handler;
    }

    start() {
        const server = http.createServer((req, res) => {
            const parsed = url.parse(req.url, true);
            const handler = this.routes[parsed.pathname];
            if (handler) {
                handler(req, res);
            } else {
                res.end("Not Found");
            }
        });
        server.listen(this.port);
    }
}

function createApp(port) {
    const app = new AppServer(port);
    app.get("/health", (req, res) => res.end("ok"));
    return app;
}

module.exports = { createApp, AppServer };
