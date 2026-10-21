import type { Task } from "./types.ts";

export function formatTask(task: Task): string {
  const mark = task.priority === "high" ? "! " : task.priority === "low" ? "(low) " : "";
  return `${task.done ? "[x]" : "[ ]"} ${mark}${task.title}`;
}

export function formatList(tasks: Task[]): string {
  return tasks.map(formatTask).join("\n");
}
