import type { Priority, Task } from "./types.ts";

const RANK: Record<Priority, number> = { high: 0, normal: 1, low: 2 };

export class TaskStore {
  private tasks: Task[] = [];
  private nextId = 1;

  add(title: string, options: { priority?: Priority } = {}): Task {
    const task: Task = { id: this.nextId++, title: title.trim(), done: false, priority: options.priority ?? "normal" };
    this.tasks.push(task);
    return task;
  }

  complete(id: number): void {
    this.find(id).done = true;
  }

  setPriority(id: number, priority: Priority): void {
    this.find(id).priority = priority;
  }

  list(): Task[] {
    return [...this.tasks].sort(
      (a, b) => Number(a.done) - Number(b.done) || RANK[a.priority] - RANK[b.priority] || a.id - b.id,
    );
  }

  private find(id: number): Task {
    const task = this.tasks.find((t) => t.id === id);
    if (!task) throw new RangeError(`no task ${id}`);
    return task;
  }
}
