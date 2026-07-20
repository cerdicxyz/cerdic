import { describe, expect, it } from "vitest";
import {
  IMR_BPS,
  LIQUIDATION_GAMMA,
  MMR_BPS,
  T1_HAIRCUT,
  T2_HAIRCUT_BPS,
  T3_HAIRCUT_BPS,
  T4_HAIRCUT_BPS,
  USYC_ARC_TESTNET,
} from "../src/constants.js";

describe("haircut ranges", () => {
  it("each tier's floor is strictly greater than the prior tier's floor", () => {
    // Floors: 0, 200, 1000, 1500. Strictly increasing — as the tier number
    // rises, the minimum haircut rises too, so no tier can quote a haircut
    // lower than a less-risky tier.
    expect(T1_HAIRCUT).toBeLessThan(T2_HAIRCUT_BPS[0]);
    expect(T2_HAIRCUT_BPS[0]).toBeLessThan(T3_HAIRCUT_BPS[0]);
    expect(T3_HAIRCUT_BPS[0]).toBeLessThan(T4_HAIRCUT_BPS[0]);
  });

  it("each tier's floor <= its own ceiling", () => {
    expect(T2_HAIRCUT_BPS[0]).toBeLessThanOrEqual(T2_HAIRCUT_BPS[1]);
    expect(T3_HAIRCUT_BPS[0]).toBeLessThanOrEqual(T3_HAIRCUT_BPS[1]);
    expect(T4_HAIRCUT_BPS[0]).toBeLessThanOrEqual(T4_HAIRCUT_BPS[1]);
  });

  it("T1 has zero haircut", () => {
    expect(T1_HAIRCUT).toBe(0);
  });

  it("T2 range is exactly 2%-5% in bps", () => {
    expect(T2_HAIRCUT_BPS).toEqual([200, 500]);
  });

  it("T3 range is exactly 10%-20% in bps", () => {
    expect(T3_HAIRCUT_BPS).toEqual([1000, 2000]);
  });

  it("T4 range is exactly 15%-35% in bps", () => {
    expect(T4_HAIRCUT_BPS).toEqual([1500, 3500]);
  });
});

describe("margin thresholds", () => {
  it("IMR is 5% (500 bps)", () => {
    expect(IMR_BPS).toBe(500);
  });

  it("MMR is 3% (300 bps)", () => {
    expect(MMR_BPS).toBe(300);
  });

  it("MMR equals 60% of IMR", () => {
    expect(MMR_BPS / IMR_BPS).toBeCloseTo(0.6, 10);
  });

  it("MMR is strictly less than IMR", () => {
    expect(MMR_BPS).toBeLessThan(IMR_BPS);
  });
});

describe("liquidation threshold", () => {
  it("gamma is pinned at 85%", () => {
    expect(LIQUIDATION_GAMMA).toBe(85);
  });

  it("gamma is within the paper's 80%-90% range", () => {
    expect(LIQUIDATION_GAMMA).toBeGreaterThanOrEqual(80);
    expect(LIQUIDATION_GAMMA).toBeLessThanOrEqual(90);
  });
});

describe("USYC Arc Testnet address", () => {
  it("is a valid 0x-prefixed 20-byte hex address", () => {
    expect(USYC_ARC_TESTNET).toMatch(/^0x[0-9a-fA-F]{40}$/);
  });

  it("matches the registered Hashnote USYC address", () => {
    expect(USYC_ARC_TESTNET.toLowerCase()).toBe(
      "0xe9185f0c5f296ed1797aae4238d26ccabeadb86c",
    );
  });
});
