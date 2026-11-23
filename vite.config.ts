import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  clearScreen: false,
  server: { port: 1430, strictPort: true },
  build: { target: "safari16", cssMinify: true },
});
