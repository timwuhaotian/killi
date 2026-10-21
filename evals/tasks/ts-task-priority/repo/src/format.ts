import type { Task } from "./types.ts";

export function formatTask(task: Task): string {
  return `${task.done ? "[x]" : "[ ]"} ${task.title}`;
}

export function formatList(tasks: Task[]): string {
  return tasks.map(formatTask).join("\n");
}
