import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      '/app': { target: 'ws://localhost:3000', ws: true },
      '/webhook': 'http://localhost:3000',
    },
  },
})
