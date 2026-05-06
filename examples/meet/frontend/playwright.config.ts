import { defineConfig } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  timeout: 30_000,
  retries: process.env.CI ? 1 : 0,
  use: {
    baseURL: 'http://localhost:3000',
    launchOptions: {
      args: [
        '--use-fake-ui-for-media-stream',
        '--use-fake-device-for-media-stream',
      ],
    },
  },
  webServer: [
    {
      command: 'cargo run -p meet',
      cwd: '../../..',
      port: 3001,
      reuseExistingServer: !process.env.CI,
      timeout: 120_000,
      env: {
        // Must match SESSION_SECRET in e2e/helpers/session.ts so JWTs
        // minted by the Playwright fetch mock validate against the
        // backend's HMAC key.
        SEAMEET_SESSION_SECRET: 'seameet-playwright-secret-32-bytes!',
        HTTP_ADDR: '0.0.0.0:3002',
      },
    },
    {
      command: 'pnpm dev',
      url: 'http://localhost:3000',
      reuseExistingServer: !process.env.CI,
      timeout: 30_000,
    },
  ],
  projects: [{ name: 'chromium', use: { browserName: 'chromium' } }],
})
