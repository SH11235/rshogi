import { defineConfig, devices } from "@playwright/test";

/**
 * @see https://playwright.dev/docs/test-configuration
 */
export default defineConfig({
    testDir: "./e2e",
    /* Run tests in files in parallel */
    fullyParallel: true,
    /* Fail the build on CI if you accidentally left test.only in the source code. */
    forbidOnly: !!process.env.CI,
    /* Retry on CI only */
    retries: process.env.CI ? 2 : 0,
    /* Opt out of parallel tests on CI. */
    workers: process.env.CI ? 1 : undefined,
    /* Reporter to use. See https://playwright.dev/docs/test-reporters */
    reporter: process.env.CI ? [["html"], ["github"]] : "html",
    /* Shared settings for all the projects below. See https://playwright.dev/docs/api/class-testoptions. */
    use: {
        /* Base URL to use in actions like `await page.goto('/')`. */
        baseURL: process.env.CI ? "http://localhost:4173" : "http://localhost:5173",

        /* Collect trace when retrying the failed test. See https://playwright.dev/docs/trace-viewer */
        trace: "on-first-retry",

        /* CI optimizations */
        video: process.env.CI ? "retain-on-failure" : "off",
        screenshot: process.env.CI ? "only-on-failure" : "off",
    },

    /* Configure projects for major browsers */
    projects: process.env.CI
        ? [
              // CI環境では主要ブラウザーのみでテスト実行時間を短縮
              {
                  name: "chromium",
                  use: { ...devices["Desktop Chrome"] },
              },
              {
                  name: "Mobile Chrome",
                  use: { ...devices["Pixel 5"] },
              },
          ]
        : [
              // ローカル環境では全ブラウザーでテスト
              {
                  name: "chromium",
                  use: { ...devices["Desktop Chrome"] },
              },

              {
                  name: "firefox",
                  use: { ...devices["Desktop Firefox"] },
              },

              {
                  name: "webkit",
                  use: { ...devices["Desktop Safari"] },
              },

              /* Test against mobile viewports. */
              {
                  name: "Mobile Chrome",
                  use: { ...devices["Pixel 5"] },
              },
              {
                  name: "Mobile Safari",
                  use: { ...devices["iPhone 12"] },
              },
          ],

    /* Run your local dev server before starting the tests */
    webServer: {
        command: "npm run dev",
        url: "http://localhost:5173",
        reuseExistingServer: !process.env.CI,
    },
});
