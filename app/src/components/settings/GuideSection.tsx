// Settings → Guide: one page explaining what hirsel is and how to drive it.
// Static prose — no wire calls, no state, nothing to save. Every claim here is
// checked against the code that implements it (keymap.ts, Composer.tsx,
// task-ref.ts, TaskShell.tsx, PhoneOverflowMenu.tsx); when the app changes, this
// page changes with it.
import { For, type JSX } from "solid-js";
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
          Home is your tasks and one conversation, nothing else. Tasks sit in a strip across the top
          on a phone and down the left on a wide screen; whichever one most needs you opens focused
          when the app loads. Work that is blocked on you comes before work that just needs a look,
          which comes before work that is only moving along. A task that arrives later never steals
          your place.
        </P>

        <SectionHeader>Tasks</SectionHeader>
        <P>
          A task is the one thing hirsel keeps for you: a piece of work with its own state, its own
          small generated interface, and the conversation that shaped it. Select a task to focus it —
          its card pins to the top and the conversation below narrows to that task. Select it again,
          or press Escape, to step back out to the ambient view where hirsel is aware of everything.
          Every task has a short ref like <span class="font-mono">#12</span>; type{" "}
          <span class="font-mono">#</span> in the composer to pick one and cite it.
        </P>

        <SectionHeader>Talking to it</SectionHeader>
        <P>
          The composer at the bottom is always there, in every state. Just type — on a desktop Enter
          sends, so there is no send button competing with the key. Paste a screenshot, or a block of
          text over 2500 characters or 30 lines, and it becomes an attachment instead of a wall in
          your message.
        </P>
        <P>
          If hirsel is mid-turn and you would rather not land on top of it, you can queue the message
          for the next one: hold the round send button on a touch screen, or press Tab on a desktop.
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
            Jump to the task list.
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
            clear the focused task.
          </Shortcut>
        </div>

        <SectionHeader>Where to poke around</SectionHeader>
        <P>
          The floating ⋯ on the home screen opens Processes: every sub-agent and monitor running
          right now, with a count on the button so you can see at a glance whether anything is. You
          cannot stop one from there — ask hirsel to stop it and it will.
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
