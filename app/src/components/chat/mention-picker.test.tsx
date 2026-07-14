import { render } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Blob } from "../../protocol";
import type { AttachmentsController } from "./useAttachments";

// Wave 1 retires the Ping slice while leaving the picker plumbing in place for
// the later event-seeded rewire. Until then no candidates or mention ids surface.
beforeEach(() => {
  vi.resetModules();
});

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

async function renderComposer(onSend: (...args: unknown[]) => void) {
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
  it("stays sourceless and sends no mention ids until the event rewire", async () => {
    const user = userEvent.setup();
    const onSend = vi.fn();
    const { textarea, container } = await renderComposer(onSend);

    await user.click(textarea);
    await user.type(textarea, "ship @release-choice");

    expect(container.querySelector('[data-slot="mention-popup"]')).toBeNull();
    expect(container.querySelector('[data-slot="mention-chips"]')).toBeNull();

    await user.keyboard("{Enter}");
    expect(onSend).toHaveBeenCalledTimes(1);
    const [body, ref, mode, blobs, mentions] = onSend.mock.calls[0];
    expect(body).toBe("ship @release-choice");
    expect(ref).toBeNull();
    expect(mode).toBe("send");
    expect(blobs).toEqual([]);
    expect(mentions).toEqual([]);
  });
});
