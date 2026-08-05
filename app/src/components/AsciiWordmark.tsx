import { useEffect, useMemo, useRef, useState } from 'react';

const DITHER_RAMP = ' .,:;irsXA253hMHGS#9B&@';
const BAYER_4X4 = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

function generateAsciiDither(cols: number, rows: number, text: string) {
  const canvas = document.createElement('canvas');
  const context = canvas.getContext('2d', { willReadFrequently: true });

  if (!context) {
    return Array.from({ length: rows }, () => ' '.repeat(cols)).join('\n');
  }

  canvas.width = cols;
  canvas.height = rows;

  context.clearRect(0, 0, cols, rows);
  context.fillStyle = '#000';
  context.fillRect(0, 0, cols, rows);
  context.fillStyle = '#fff';
  context.textAlign = 'center';
  context.textBaseline = 'middle';
  context.font = `700 ${Math.max(12, Math.floor(rows * 0.78))}px 'Courier New', monospace`;
  context.fillText(text, cols / 2, rows / 2 + rows * 0.02);

  const image = context.getImageData(0, 0, cols, rows).data;

  return Array.from({ length: rows }, (_, y) => {
    return Array.from({ length: cols }, (_, x) => {
      const index = (y * cols + x) * 4;
      const brightness = image[index] ?? 0;
      const threshold = (BAYER_4X4[(y % 4) * 4 + (x % 4)] + 0.5) / 16;
      const dithered = Math.max(0, Math.min(1, brightness / 255 + (threshold - 0.5) * 0.9));
      const rampIndex = Math.min(DITHER_RAMP.length - 1, Math.floor(dithered * (DITHER_RAMP.length - 1)));

      return DITHER_RAMP[rampIndex] ?? ' ';
    }).join('');
  }).join('\n');
}

export function AsciiWordmark({ text = 'cerdic' }: { text?: string }) {
  const frameRef = useRef<HTMLDivElement | null>(null);
  const [ascii, setAscii] = useState('');

  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;

    const update = () => {
      const width = frame.clientWidth;
      const height = frame.clientHeight;
      const cols = Math.max(48, Math.floor(width / 9));
      const rows = Math.max(18, Math.floor(height / 18));
      setAscii(generateAsciiDither(cols, rows, text));
    };

    update();
    const resizeObserver = new ResizeObserver(() => update());
    resizeObserver.observe(frame);
    return () => resizeObserver.disconnect();
  }, [text]);

  const lines = useMemo(() => ascii.split('\n'), [ascii]);

  return (
    <div ref={frameRef} className="ascii-frame" aria-hidden="true">
      <pre className="ascii-dither">{lines.join('\n')}</pre>
    </div>
  );
}
