const { defineConfig, devices } = require("@playwright/test");

module.exports = defineConfig({
  testDir: "./tests",
  timeout: 15_000,
  expect: { timeout: 5_000 },
  reporter: "list",
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
