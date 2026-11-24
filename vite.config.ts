import { defineConfig, type Plugin } from "vite";
import solid from "@solidjs/vite-plugin";

/** Dev only: answers /api/* from src/phone/devMock.ts so phone.html runs without the Mac. */
function phoneMock(): Plugin {
  return {
    name: "phone-dev-mock",
    apply: "serve",
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        if (!req.url?.startsWith("/api/")) return next();
        try {
          let raw = "";
          for await (const chunk of req) raw += chunk;
          const mock = await server.ssrLoadModule("/src/phone/devMock.ts");
          const auth = req.headers.authorization;
          const { status, body } = await mock.handle(req.method ?? "GET", req.url, auth, raw ? JSON.parse(raw) : undefined);
          res.statusCode = status;
          res.setHeader("Content-Type", "application/json");
          res.end(JSON.stringify(body));
        } catch (e) {
          res.statusCode = 500;
          res.end(JSON.stringify({ error: String(e) }));
        }
      });
    },
  };
}

export default defineConfig({
  plugins: [solid(), phoneMock()],
  clearScreen: false,
  server: { port: 1430, strictPort: true },
  build: {
    target: "safari16",
    cssMinify: true,
    rollupOptions: { input: { main: "index.html", phone: "phone.html" } },
  },
});
