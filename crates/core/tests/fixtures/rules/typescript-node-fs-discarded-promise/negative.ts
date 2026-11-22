import { rename as stage } from 'node:fs/promises';

export function relocate(source: string, target: string): Promise<void> {
  return stage(source, target);
}

export function preview(stage: (name: string) => void) {
  stage('preview');
}
