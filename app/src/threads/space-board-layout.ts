export const SPACE_CONVERSATION_MIN_REM = 26;
export const SPACE_BOARD_MIN_REM = 20;
export const SPACE_PANE_GAP_REM = 0.5;

/** A Space may place its board beside the conversation only when both panes
 * keep their useful minimum measure. The available width is the Space's real
 * in-flow allocation, after inventory, previews, showcases and utilities have
 * taken their slots. */
export function spaceBoardFitsBesideConversation(availablePx: number, remPx = 16): boolean {
  const requiredRem = SPACE_CONVERSATION_MIN_REM + SPACE_BOARD_MIN_REM + SPACE_PANE_GAP_REM;
  return availablePx >= requiredRem * remPx;
}
