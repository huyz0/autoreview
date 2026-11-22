function hasActive(tasks: readonly { active: boolean }[]) {
  return tasks.filter(task => task.active).length !== 0;
}
