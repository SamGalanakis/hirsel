import type { EventItem } from "../../protocol";

export interface TaskNavigationState {
  focusedId: number | null;
}

export const initialTaskNavigation: TaskNavigationState = {
  focusedId: null,
};

export function toggleTaskFocus(
  state: TaskNavigationState,
  task: EventItem,
): TaskNavigationState {
  return { focusedId: state.focusedId === task.id ? null : task.id };
}

export function reconcileTaskNavigation(
  state: TaskNavigationState,
  tasks: EventItem[],
): TaskNavigationState {
  if (state.focusedId === null) return state;
  const ids = new Set(tasks.map((task) => task.id));
  return ids.has(state.focusedId) ? state : initialTaskNavigation;
}
