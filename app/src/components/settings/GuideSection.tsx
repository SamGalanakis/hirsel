// Settings → Guide: one page explaining what hirsel is and how to drive it.
// Static prose — no wire calls, no state, nothing to save. Every claim here is
// checked against the code that implements it (keymap.ts, Composer.tsx,
// task-ref.ts, ThreadShell.tsx, ThreadNavigation.tsx); when the app changes, this
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

        <SectionHeader>The home screen</SectionHeader>
        <P>
          Home is your global conversation with Hirsel. The icon rail opens Threads, creates a
          new Thread, lists all artifacts, and opens Processes or Settings. The Thread drawer is
          closed until you need it. Opening a Thread gives its conversation a frame and a compact
          context strip; the Home button in the rail returns to your global conversation.
        </P>

        <SectionHeader>Threads</SectionHeader>
        <P>
          Each Thread has its own conversation, state and any generated interface needed for the
          work. Messages go to the Thread named in the context strip. Opening a Thread marks it
          read; settling it is a separate action in its menu. Unread activity, work that needs your
          input, and active execution are independent signals in the drawer. Every Thread has a
          short reference such as <span class="font-mono">#12</span>; type{" "}
          <span class="font-mono">#</span> in a message to cite another Thread without moving your conversation.
        </P>

        <P>
          Thread rows show working time, queued work and the latest turn outcome. Turn finished does not settle a Thread. Use its check button or action menu to settle explicitly; the menu also offers snooze, archive and a Thread link.
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
          the conversation or use the document button for that Thread's artifacts. The grid button
          in the rail lists all artifacts while keeping your current conversation addressed. Threads
          can reference the same artifact; each reference opens its current content.
        </P>
        <P>
          An artifact opens beside your conversation on desktop and full-screen on phone. Close it
          to return to your draft. Detailed tool activity stays under Inspect execution, with useful
          updates and results in the conversation.
        </P>

        <SectionHeader>The agents</SectionHeader>
        <P>
          There is one main agent, and that is who you are always talking to. Anything that arrives
          on its own — a sub-agent finishing, a monitor firing — is triaged first by a short-lived
          fork agent, so the main one is only interrupted when something genuinely needs it, and
          bigger jobs get handed off to sub-agents, which do work but never speak to you. You can
          change the model behind each of them under Settings → Agents, and the accounts and keys
          they run on under Settings → Providers. The defaults are fine.
        </P>

        <SectionHeader>Keyboard, on a desktop</SectionHeader>
        <P>
          Everything below is also in the command palette, and the full sheet is one keypress away.
          Single-key shortcuts stand down while you are typing.
        </P>
        <div class="mt-2 divide-y divide-border">
          <Shortcut keys={["⌘/Ctrl", "K"]}>The command palette. Start here — it holds the lot.</Shortcut>
          <Shortcut keys={["⌘/Ctrl", "/"]}>The keyboard shortcut sheet.</Shortcut>
          <Shortcut keys={["g", "t"]} chord>
            Open the Thread drawer.
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
            Back out, one rung at a time: close whatever is open, else stop the running turn, else
            return from the focused Thread to Home.
          </Shortcut>
        </div>

        <SectionHeader>Where to poke around</SectionHeader>
        <P>
          The activity icon in the rail opens Processes, where you can inspect sub-agents and
          monitors. Ask Hirsel to stop a process when you no longer need it.
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
