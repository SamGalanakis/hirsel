import { render, screen, waitFor } from "@solidjs/testing-library";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "./dropdown-menu";

function Fixture(props: { selected: () => void }) {
  return <DropdownMenu>
    <DropdownMenuTrigger>Actions</DropdownMenuTrigger>
    <DropdownMenuContent>
      <DropdownMenuItem onSelect={props.selected}>Processes</DropdownMenuItem>
      <DropdownMenuItem disabled>Unavailable</DropdownMenuItem>
      <DropdownMenuItem>Settings</DropdownMenuItem>
    </DropdownMenuContent>
  </DropdownMenu>;
}
describe("overflow menu keyboard contract", () => {
  it("opens from ArrowDown, skips disabled rows, and restores focus on Escape", async () => {
    const user = userEvent.setup();
    render(() => <Fixture selected={() => {}} />);
    const trigger = screen.getByRole("button", {name: "Actions"});
    trigger.focus();
    await user.keyboard("{ArrowDown}");
    await waitFor(() => expect(screen.getByRole("menuitem", {name: "Processes"})).toHaveFocus());
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("menuitem", {name: "Settings"})).toHaveFocus();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    expect(trigger).toHaveFocus();
  });
  it("selects exactly once and closes the menu", async () => {
    const selected = vi.fn();
    const user = userEvent.setup();
    render(() => <Fixture selected={selected} />);
    await user.click(screen.getByRole("button", {name: "Actions"}));
    await user.click(await screen.findByRole("menuitem", {name: "Processes"}));
    expect(selected).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("menu")).toBeNull();
  });
});
