import { describe, expect, it } from "vitest";
import {
  SPACE_BOARD_MIN_REM,
  SPACE_CONVERSATION_MIN_REM,
  SPACE_PANE_GAP_REM,
  spaceBoardFitsBesideConversation,
} from "./space-board-layout";

describe("Space pane budget", () => {
  it("reserves the conversation, board and their gap before showing both panes", () => {
    const required = (SPACE_CONVERSATION_MIN_REM + SPACE_BOARD_MIN_REM + SPACE_PANE_GAP_REM) * 16;
    expect(required).toBe(744);
    expect(spaceBoardFitsBesideConversation(required - 1)).toBe(false);
    expect(spaceBoardFitsBesideConversation(required)).toBe(true);
  });

  it("uses the document rem size for zoomed layout arithmetic", () => {
    expect(spaceBoardFitsBesideConversation(929, 20)).toBe(false);
    expect(spaceBoardFitsBesideConversation(930, 20)).toBe(true);
  });
});
