// Settings → Guide: one page explaining what hirsel is and how to drive it.
// Static prose — no wire calls, no state, nothing to save. Every claim here is
// checked against the code that implements it (keymap.ts, Composer.tsx,
// thread-ref.ts, ThreadShell.tsx, ThreadNavigation.tsx); when the app changes, this
// page changes with it.
import { For } from "solid-js";
import { type JSX } from "@solidjs/web";
import { Group, SectionHeader } from "./rows";

/** A paragraph of guide prose, at the body measure the rest of Settings reads
 * at. Muted, because this is explanation rather than a value the Owner set. */
function P(props: { children: JSX.Element }) {
  return <p class="mt-2 text-sm leading-relaxed text-muted-foreground">{props.children}</p>;
}

/** A keyboard token, same mono chip the command palette renders its hints as. */
function Key(props: { children: JSX.Element }) {
  return (
    <kbd class="grid h-5 min-w-5 place-items-center rounded-sm border border-border bg-muted px-1 font-mono text-meta text-foreground/90">
      {props.children}
    </kbd>
  );
}

/** One shortcut: its keys, then what they do. A chord renders as two chips with
 * "then" between them, the way the palette spells `g` `t`. */
function Shortcut(props: { keys: string[]; chord?: boolean; children: JSX.Element }) {
  return (
    <div class="flex items-baseline gap-3 py-2">
      <span class="flex shrink-0 items-center gap-1">
        <For each={props.keys}>
          {(k, i) => (
            <>
              {props.chord && i() > 0 ? (
                <span class="text-meta text-muted-foreground">then</span>
              ) : null}
              <Key>{k}</Key>
            </>
          )}
        </For>
      </span>
      <span class="min-w-0 text-sm leading-relaxed text-muted-foreground">{props.children}</span>
    </div>
  );
}

export function GuideSection(): JSX.Element {
  return (
    <>
      <Group>
        <SectionHeader>What hirsel is</SectionHeader>
        <P>
          Hirsel is your own agent, running on your own machine. It keeps working while you are
          away — starting jobs, watching things, deciding what can wait. You talk to it here, and
          everything that happens comes back to this one place.
        </P>

        <SectionHeader>Your workspace</SectionHeader>
        <P>
          The icon rail opens Spaces and Tasks, creates either kind, lists all artifacts, and opens
          Processes or Settings. Choose or create a Space or Task before composing. Each selected
          conversation has a frame and a named context strip. The overview returns to Thread
          selection without addressing a message.
        </P>

        <SectionHeader>Spaces and Tasks</SectionHeader>
        <P>
          Each Space or Task has its own conversation, state and any generated interface needed for the
          work. Spaces hold ongoing context and can contain Spaces or Tasks. Tasks hold finishable work and can contain Tasks. Pin either kind for quick
          access; pinning and parentage are independent of completion and visibility. Child
          progress and results return to their parent with links to the source conversation. Messages go to the Thread named in the context strip. Opening a Thread marks it
          read; marking a Task done is a separate action in its menu. Unread activity, work that needs your
          input, and active execution are independent signals in the drawer. A dot on Spaces and Tasks means a visible item needs you. Use the filter menu for done, snoozed or archived work, and Search to find a conversation by title or reference. Every conversation has a
          short reference such as <span class="font-mono">#12</span>; type{" "}
          <span class="font-mono">#</span> in a message to cite another Thread without moving your conversation.
        </P>

        <P>
          Rows show working time, queued work and failures. Tasks also show their latest successful turn outcome and completion state. Use a Task’s check button or action menu to mark it done explicitly; the menu also offers snooze, archive and a Thread link.
        </P>
        <SectionHeader>Talking to it</SectionHeader>
        <P>
          Type in the composer and select Send. On desktop, Enter also sends and Shift+Enter adds a
          line; on touch, Enter adds a line. Paste a screenshot, or text over 2500 characters or
          30 lines, to stage it as an attachment. Drafts stay with their Thread when you switch views.
        </P>
        <P>
          Send remains available while Hirsel is working, alongside Stop. To queue a message for
          the next turn, hold Send on touch or press Ctrl/Cmd+Shift+Enter. Stop interrupts
          the active turn in the addressed Thread.
        </P>

        <SectionHeader>Artifacts and execution</SectionHeader>
        <P>
          Hirsel can create interactive previews, pages and files as artifacts. Open a reference in
          the conversation or use Related for that Thread’s artifacts and saved links and threads. The grid button
          in the rail lists all artifacts while keeping your current conversation addressed. Threads
          can reference the same artifact; each reference opens its current content. Previewing a
          result stages an About context in your selected Thread’s draft. Remove it to send without
          that reference, or keep it when asking Hirsel to change the result. Closing the preview
          keeps this context; switching Threads never transfers it.
        </P>
        <P>
          Use a link’s adjacent actions to open it, copy its address or save it to Related. Saving a
          link keeps it with that Thread without sending a message. Remove a saved link from its
          row’s actions; artifacts keep their existing conversation references.
        </P>
        <P>
          An artifact opens beside your conversation on desktop and full-screen on phone. Close it
          to return to your draft. Expand a work summary to see its steps, with useful
          updates and results in the conversation.
        </P>

        <SectionHeader>The agents</SectionHeader>
        <P>
          Each Thread keeps its own conversation context. A coordinator can create focused child
          Threads and receive their progress and results. You can open any parent or child and
          talk there directly. Change shared model defaults and available delegation models
          under Settings → Thread models, and configure their accounts under Settings → Providers.
        </P>

        <SectionHeader>Keyboard, on a desktop</SectionHeader>
        <P>
          Everything below is also in the command palette, and the full sheet is one keypress away.
          Single-key shortcuts stand down while you are typing.
        </P>
        <div class="mt-2 divide-y divide-border">
          <Shortcut keys={["⌘/Ctrl", "K"]}>Search commands, Spaces and Tasks by title or #reference.</Shortcut>
          <Shortcut keys={["⌘/Ctrl", "/"]}>The keyboard shortcut sheet.</Shortcut>
          <Shortcut keys={["g", "t"]} chord>
            Open Spaces and Tasks.
          </Shortcut>
          <Shortcut keys={["g", "h"]} chord>
            Jump back to the composer.
          </Shortcut>
          <Shortcut keys={["g", "p"]} chord>
            Open Processes.
          </Shortcut>
          <Shortcut keys={["g", "s"]} chord>
            Open Settings.
          </Shortcut>
          <Shortcut keys={["/"]}>Focus the composer, same as g then h.</Shortcut>
          <Shortcut keys={["G"]}>Jump down to the latest message.</Shortcut>
          <Shortcut keys={["Esc"]}>
            Close the active picker, menu or preview. Escape never changes the conversation
            addressed by your draft. Use Stop to interrupt execution.
          </Shortcut>
        </div>

        <SectionHeader>Where to poke around</SectionHeader>
        <P>
          The activity icon in the rail opens Processes, where you can inspect monitors and their
          latest summaries.
        </P>
        <P>
          Settings → Agents is where the models, reasoning levels and system prompts live, and
          Settings → Providers is where the accounts and API keys behind them do. About & debug has
          versions and a copyable diagnostics blob if something looks wrong.
        </P>
      </Group>
    </>
  );
}
