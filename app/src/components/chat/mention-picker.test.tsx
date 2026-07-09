import { render } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Blob, Ping } from "../../protocol";
import type { AttachmentsController } from "./useAttachments";

// The @-mention picker (v2.1): typing `@` opens a quick-select of open Pings;
// picking one inserts an `@handle` token AND the sent frame carries the ping id
// in `mentions`. Each test runs against a pristine store singleton.
beforeEach(() => {
  vi.resetModules();
});

function ping(overrides: Partial<Ping> = {}): Ping {
  return {
    id: 7,
    name: "release-choice",
    description: "Choose the release channel",
    content: "Which channel should we ship?",
    anchor: 5,
    requires_response: true,
    quick_replies: [],
    status: "open",
    ts: "2026-07-10T00:00:00Z",
    ...overrides,
  };
}

function stubAttachments(): AttachmentsController {
  return {
    files: () => [],
    addFiles: () => {},
    removeFile: () => {},
    retry: () => {},
    clear: () => {},
    uploadAll: async () => [] as Blob[],
  };
}

async function seedAndRender(pings: Ping[], onSend: (...args: unknown[]) => void) {
  const store = await import("../../store/store");
  for (const p of pings) {
    store.dispatch({ type: "ping_upsert", payload: { type: "ping_upsert", ping: p } });
  }
  const { Composer } = await import("./Composer");
  const utils = render(() => (
    <Composer
      replyingTo={null}
      onCancelReply={() => {}}
      attachments={stubAttachments()}
      thinking={false}
      onSend={onSend as never}
      onStop={() => {}}
      getLastOwnerBody={() => null}
    />
  ));
  const textarea = utils.container.querySelector(
    '[data-composer="main"]',
  ) as HTMLTextAreaElement;
  return { ...utils, textarea };
}

describe("mention picker (desktop, keyboard-first)", () => {
  it("opens on @, inserts the @handle token, and sends the ping id in mentions", async () => {
    const user = userEvent.setup();
    const onSend = vi.fn();
    const { textarea, queryByText, container } = await seedAndRender(
      [ping({ id: 7, name: "release-choice" }), ping({ id: 8, name: "deploy-window" })],
      onSend,
    );

    await user.click(textarea);
    await user.type(textarea, "ship @rel");

    // The popup surfaces the matching open Ping by @name + description.
    const popup = container.querySelector('[data-slot="mention-popup"]');
    expect(popup).not.toBeNull();
    expect(queryByText("Choose the release channel")).not.toBeNull();

    // Enter accepts the active row while the picker is open (does NOT send).
    await user.keyboard("{Enter}");
    expect(onSend).not.toHaveBeenCalled();
    expect(textarea.value).toBe("ship @release-choice ");
    // Picker closed after accept.
    expect(container.querySelector('[data-slot="mention-popup"]')).toBeNull();

    // A second Enter (picker closed) sends; the frame carries mentions=[7].
    await user.keyboard("{Enter}");
    expect(onSend).toHaveBeenCalledTimes(1);
    const [body, ref, mode, blobs, mentions] = onSend.mock.calls[0];
    expect(body).toBe("ship @release-choice");
    expect(ref).toBeNull();
    expect(mode).toBe("send");
    expect(blobs).toEqual([]);
    expect(mentions).toEqual([7]);
  });

  it("navigates candidates with ArrowDown before accepting", async () => {
    const user = userEvent.setup();
    const onSend = vi.fn();
    const { textarea } = await seedAndRender(
      [
        ping({ id: 1, name: "release-notes" }),
        ping({ id: 2, name: "release-choice" }),
      ],
      onSend,
    );

    await user.click(textarea);
    await user.type(textarea, "@release");
    // Two startsWith matches, newest-first: [release-choice(2), release-notes(1)].
    await user.keyboard("{ArrowDown}"); // move to the second row
    await user.keyboard("{Enter}"); // accept it
    expect(textarea.value.trim()).toBe("@release-notes");
    await user.keyboard("{Enter}"); // send

    expect(onSend.mock.calls[0][0]).toBe("@release-notes");
    expect(onSend.mock.calls[0][4]).toEqual([1]);
  });

  it("dropping the @token from the text drops it from mentions", async () => {
    const user = userEvent.setup();
    const onSend = vi.fn();
    const { textarea } = await seedAndRender([ping({ id: 7, name: "release-choice" })], onSend);

    await user.click(textarea);
    await user.type(textarea, "@rel");
    await user.keyboard("{Enter}"); // accept -> "@release-choice "
    // Clear it back out entirely, type a plain message.
    await user.clear(textarea);
    await user.type(textarea, "never mind");
    await user.keyboard("{Enter}"); // send

    expect(onSend.mock.calls[0][0]).toBe("never mind");
    expect(onSend.mock.calls[0][4]).toEqual([]);
  });
});

describe("mention picker (phone, quick-select chips)", () => {
  it("surfaces a chip row on @ and inserts the handle on tap", async () => {
    // Force coarse-pointer (touch) for this test.
    window.matchMedia = ((query: string) => ({
      matches: query.includes("coarse"),
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    })) as typeof window.matchMedia;

    const user = userEvent.setup();
    const onSend = vi.fn();
    const { textarea, container, getByText } = await seedAndRender(
      [ping({ id: 7, name: "release-choice" })],
      onSend,
    );

    await user.click(textarea);
    await user.type(textarea, "@");

    const chips = container.querySelector('[data-slot="mention-chips"]');
    expect(chips).not.toBeNull();
    // Tap the chip.
    await user.click(getByText("release-choice"));
    expect(textarea.value).toBe("@release-choice ");

    // Reset matchMedia for the rest of the suite.
    window.matchMedia = ((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    })) as typeof window.matchMedia;
  });
});
