import { createRoot } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";
import { createFocusTrap } from "./focus";

afterEach(() => {
  document.body.replaceChildren();
});

function visibleBox(element: HTMLElement): void {
  Object.defineProperty(element, "getClientRects", {
    configurable: true,
    value: () => [{ width: 44, height: 44 }],
  });
}

describe("focus trap visibility", () => {
  it("starts on the first visible control and recovers from a hidden active sibling", async () => {
    const panel = document.createElement("div");
    panel.tabIndex = -1;
    const hiddenDesktop = document.createElement("button");
    hiddenDesktop.style.display = "none";
    hiddenDesktop.textContent = "Desktop close";
    const phoneBack = document.createElement("button");
    phoneBack.textContent = "Phone back";
    visibleBox(phoneBack);
    panel.append(hiddenDesktop, phoneBack);
    document.body.append(panel);

    let dispose = () => {};
    createRoot((rootDispose) => {
      dispose = rootDispose;
      createFocusTrap(() => panel);
    });
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(phoneBack).toHaveFocus();

    hiddenDesktop.focus();
    expect(hiddenDesktop).toHaveFocus();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab" }));
    expect(phoneBack).toHaveFocus();
    dispose();
  });

  it("owns each modal Tab step and repairs focus when a live row is replaced", async () => {
    const panel = document.createElement("div");
    panel.tabIndex = -1;
    const first = document.createElement("button");
    first.textContent = "First";
    const liveRow = document.createElement("button");
    liveRow.textContent = "Live row";
    const last = document.createElement("button");
    last.textContent = "Last";
    for (const button of [first, liveRow, last]) visibleBox(button);
    panel.append(first, liveRow, last);
    const background = document.createElement("button");
    background.textContent = "Background";
    visibleBox(background);
    document.body.append(panel, background);

    let dispose = () => {};
    createRoot((rootDispose) => {
      dispose = rootDispose;
      createFocusTrap(() => panel);
    });
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(first).toHaveFocus();

    window.dispatchEvent(new KeyboardEvent("keydown", {
      key: "Tab",
      bubbles: true,
      cancelable: true,
    }));
    expect(liveRow).toHaveFocus();
    liveRow.remove();
    await Promise.resolve();
    await Promise.resolve();
    expect(first).toHaveFocus();

    first.focus();
    window.dispatchEvent(new KeyboardEvent("keydown", {
      key: "Tab",
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    }));
    expect(last).toHaveFocus();
    expect(background).not.toHaveFocus();
    dispose();
  });

  it("prevents later overlay handlers from restoring focus outside after Tab", async () => {
    const panel = document.createElement("div");
    panel.tabIndex = -1;
    const first = document.createElement("button");
    first.textContent = "First";
    const second = document.createElement("button");
    second.textContent = "Second";
    const background = document.createElement("button");
    background.textContent = "Background";
    for (const button of [first, second, background]) visibleBox(button);
    panel.append(first, second);
    document.body.append(panel, background);

    let dispose = () => {};
    createRoot((rootDispose) => {
      dispose = rootDispose;
      createFocusTrap(() => panel);
    });
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(first).toHaveFocus();

    const competingRestore = (event: KeyboardEvent) => {
      if (event.key === "Tab") background.focus();
    };
    window.addEventListener("keydown", competingRestore);
    first.dispatchEvent(new KeyboardEvent("keydown", {
      key: "Tab",
      bubbles: true,
      cancelable: true,
    }));

    expect(second).toHaveFocus();
    expect(background).not.toHaveFocus();
    window.removeEventListener("keydown", competingRestore);
    dispose();
  });

  it("reclaims late focus restoration outside an open modal by the next paint", async () => {
    const panel = document.createElement("div");
    panel.tabIndex = -1;
    const close = document.createElement("button");
    close.textContent = "Close";
    const background = document.createElement("button");
    background.textContent = "Background trigger";
    visibleBox(close);
    visibleBox(background);
    panel.append(close);
    document.body.append(panel, background);

    let dispose = () => {};
    createRoot((rootDispose) => {
      dispose = rootDispose;
      createFocusTrap(() => panel);
    });
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(close).toHaveFocus();

    background.focus();
    expect(background).toHaveFocus();
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(close).toHaveFocus();
    dispose();
  });
});
