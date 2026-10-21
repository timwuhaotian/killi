export type Priority = "high" | "normal" | "low";

export interface Task {
  id: number;
  title: string;
  done: boolean;
  priority: Priority;
}
