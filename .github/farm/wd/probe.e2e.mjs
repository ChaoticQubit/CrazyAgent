import assert from "node:assert/strict";

describe("the Windows farm probe", () => {
  it("reads the title, types into the page and reads the page's echo", async () => {
    assert.equal(await browser.getTitle(), "farm");
    const input = await $("#probe-input");
    await input.click();
    await browser.keys(["x", "y", "z"]);
    assert.equal(await input.getValue(), "xyz");
    assert.equal(await $("#echo").getText(), "xyz");
    const shot = await browser.takeScreenshot();
    assert.ok(shot.length > 1000, "a screenshot of the window");
    const image = process.env.ImageOS;
    console.log(`wdio: title, keys, echo and a ${shot.length} byte screenshot on ${image}`);
  });
});
