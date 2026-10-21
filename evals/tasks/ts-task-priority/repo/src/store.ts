import type { Task } from "./types.ts";

export class TaskStore {
  private tasks: Task[] = [];
  private nextId = 1;

  add(title: string): Task {
    const task: Task = { id: this.nextId++, title: title.trim(), done: false };
    this.tasks.push(task);
    return task;
  }

  complete(id: number): void {
    const task = this.tasks.find((t) => t.id === id);
    if (!task) throw new RangeError(`no task ${id}`);
    task.done = true;
  }

  /** Open tasks first, then done ones, each in the order they were added. */
  list(): Task[] {
    return [...this.tasks].sort((a, b) => Number(a.done) - Number(b.done) || a.id - b.id);
  }
}
