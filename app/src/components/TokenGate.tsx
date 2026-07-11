import { createSignal, onMount } from "solid-js";
import { Button } from "./ui/button";
import { Input } from "./ui/input";

interface Props {
  onSubmit: (token: string) => void;
}

/** First-run prompt for the bearer token (protocol.md Auth). Persisted to
 * localStorage by the caller once submitted; this component only collects it. */
export function TokenGate(props: Props) {
  const [value, setValue] = createSignal("");
  let inputRef: HTMLInputElement | undefined;

  onMount(() => inputRef?.focus());

  function handleSubmit(e: SubmitEvent) {
    e.preventDefault();
    const trimmed = value().trim();
    if (trimmed.length === 0) return;
    props.onSubmit(trimmed);
  }

  return (
    <div class="flex flex-1 flex-col items-center justify-center gap-4 p-6 text-center">
      <h1 class="m-0 text-base font-semibold tracking-[0.01em]">hirsel</h1>
      <p class="m-0 max-w-[32ch] text-[0.95rem] text-muted-foreground">
        Enter the access token for your Hirsel Host. It's stored only on this device.
      </p>
      <form class="flex w-full max-w-[320px] flex-col gap-3" onSubmit={handleSubmit}>
        <Input
          ref={inputRef}
          type="password"
          inputMode="text"
          autocomplete="off"
          autocapitalize="off"
          autocorrect="off"
          spellcheck={false}
          placeholder="access token"
          aria-label="Access token"
          class="h-11 text-center text-base"
          value={value()}
          onInput={(e) => setValue(e.currentTarget.value)}
        />
        <Button type="submit" size="lg" class="h-11 text-base" disabled={value().trim().length === 0}>
          Connect
        </Button>
      </form>
    </div>
  );
}
