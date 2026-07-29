import React, { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { X } from "lucide-react";
import type { FileTranscriptionJob } from "@/bindings";
import { ProgressBar } from "../../shared";

interface FileJobRowProps {
  job: FileTranscriptionJob;
  isSelected: boolean;
  onSelect: () => void;
  onCancel: () => void;
}

const isRunning = (job: FileTranscriptionJob) =>
  job.status === "queued" ||
  job.status === "decoding" ||
  job.status === "transcribing";

const formatDuration = (secs: number): string => {
  const total = Math.round(secs);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
};

export const FileJobRow: React.FC<FileJobRowProps> = ({
  job,
  isSelected,
  onSelect,
  onCancel,
}) => {
  const { t } = useTranslation();

  const percentage =
    job.chunks_total > 0 ? (job.chunks_done / job.chunks_total) * 100 : 0;

  const handleCancel = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      onCancel();
    },
    [onCancel],
  );

  return (
    <div
      className={`flex items-center gap-3 p-3 rounded-lg cursor-pointer transition-colors ${
        isSelected ? "bg-logo-primary/20" : "hover:bg-mid-gray/10"
      }`}
      onClick={onSelect}
    >
      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium truncate" title={job.file_name}>
          {job.file_name}
        </p>
        <p className="text-xs text-text/60">
          {job.duration_secs > 0 && (
            <span className="me-2">{formatDuration(job.duration_secs)}</span>
          )}
          <span>{t(`files.status.${job.status}`)}</span>
        </p>
      </div>

      {job.status === "transcribing" && (
        <ProgressBar
          progress={[{ id: job.id, percentage }]}
          size="medium"
          showLabel
        />
      )}

      {isRunning(job) && (
        <button
          className="p-1 rounded hover:bg-mid-gray/20 shrink-0 cursor-pointer text-text/50 hover:text-logo-primary transition-colors"
          title={t("files.cancel")}
          onClick={handleCancel}
        >
          <X width={16} height={16} />
        </button>
      )}
    </div>
  );
};
