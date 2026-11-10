import { describe, expect, it } from "vitest";
import { suggestIdentityUsername } from "./identityUsername";

describe("identity username suggestion", () => {
  it("normalizes account name and picks next free variant", () => {
    expect(suggestIdentityUsername("Kids Wallet", [])).toBe("kids-wallet");
    expect(suggestIdentityUsername("Kids Wallet", ["kids-wallet", "kids-wallet-2"])).toBe("kids-wallet-3");
    expect(suggestIdentityUsername("Admin", [])).toBe("admin-2");
  });
});
