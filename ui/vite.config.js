import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// Tauri asset 协议需要相对路径
export default defineConfig({
  plugins: [vue()],
  base: "./",
});
