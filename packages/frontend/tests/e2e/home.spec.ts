import { test, expect } from "@playwright/test";

test.describe("synchra/frontend smoke", () => {
  test("home page renders Synchra heading", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByRole("heading", { name: "Synchra" })).toBeVisible();
  });
});
