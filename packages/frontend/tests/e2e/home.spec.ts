import { test, expect } from "@playwright/test";

test.describe("cerdic/frontend smoke", () => {
  test("home page renders Cerdic heading", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByRole("heading", { name: "Cerdic" })).toBeVisible();
  });
});
