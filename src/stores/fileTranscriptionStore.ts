import { create } from "zustand";
import { commands, events, type FileTranscriptionJob } from "@/bindings";

interface FileTranscriptionState {
  jobs: FileTranscriptionJob[];
  selectedJobId: string | null;
  /** Message d'erreur de mise en file (format non supporté, etc.). */
  enqueueError: string | null;
  refresh: () => Promise<void>;
  enqueue: (paths: string[]) => Promise<void>;
  cancel: (jobId: string) => Promise<void>;
  select: (jobId: string | null) => void;
  clearEnqueueError: () => void;
  subscribe: () => Promise<() => void>;
}

export const useFileTranscriptionStore = create<FileTranscriptionState>(
  (set, get) => ({
    jobs: [],
    selectedJobId: null,
    enqueueError: null,

    refresh: async () => {
      const jobs = await commands.listFileTranscriptionJobs();
      set({ jobs });
    },

    enqueue: async (paths) => {
      set({ enqueueError: null });
      const result = await commands.enqueueFileTranscriptions(paths);
      if (result.status === "error") {
        set({ enqueueError: result.error });
        return;
      }
      await get().refresh();
      // Sélectionner le premier fichier ajouté évite un panneau de résultat
      // vide juste après un dépôt.
      const first = result.data[0];
      if (first && !get().selectedJobId) {
        set({ selectedJobId: first.id });
      }
    },

    cancel: async (jobId) => {
      await commands.cancelFileTranscription(jobId);
      await get().refresh();
    },

    select: (jobId) => set({ selectedJobId: jobId }),

    clearEnqueueError: () => set({ enqueueError: null }),

    subscribe: async () => {
      const unlisten = await events.fileTranscriptionProgress.listen((e) => {
        const incoming = e.payload.job;
        set((state) => {
          const jobs = state.jobs.some((j) => j.id === incoming.id)
            ? state.jobs.map((j) => (j.id === incoming.id ? incoming : j))
            : [...state.jobs, incoming];
          return { jobs };
        });
      });
      return unlisten;
    },
  }),
);
