import { describe, expect, it } from "vitest";
import { explorerLink } from "./NodeLabel";

describe("explorerLink", () => {
  const key = "03864ef025fde8fb587d989186ce6a4a186895ee44a926bfc370e2c366597a3f8f";

  it("fills the pubkey into the template", () => {
    expect(explorerLink("https://amboss.space/node/{pubkey}", key)).toBe(`https://amboss.space/node/${key}`);
  });

  it("shows no link without a template or for non-http schemes", () => {
    expect(explorerLink(undefined, key)).toBeNull();
    expect(explorerLink("javascript:alert(1)//{pubkey}", key)).toBeNull();
  });
});
