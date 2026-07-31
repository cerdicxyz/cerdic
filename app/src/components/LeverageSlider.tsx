import { useCallback, useEffect, useRef, useState } from 'react';
import { AnimatePresence, MotionConfig, animate, motion, useMotionValue, useReducedMotion } from 'framer-motion';

// Ported from cer-perp's trading panel (see app/design.md's "why this
// palette" note): a bar-chart ramp, not a flat <input type="range">, per
// design.md's Leverage control anti-patterns.

const MIN_LEV = 1;
// Drives bar heights and tick marks — dense on purpose, the ramp reads as a
// continuous shape.
const BAR_MARKS = [1, 3, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50];
// Drives the clickable text row underneath. A trade panel this narrow can't
// fit "1x 3x 5x" without the labels colliding, so this is a thinned subset
// of BAR_MARKS, not the same list — 3x is still a real step (drag or arrow
// keys reach it), it just isn't its own button.
const STEP_LABELS = [1, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50];
const DEFAULT_MAX = 50;

function barHeight(step: number, totalSteps: number) {
  return 10 + (step / totalSteps) * 54;
}

export interface LeverageSliderProps {
  value: number;
  onChange: (value: number) => void;
  maxValue?: number;
}

export function LeverageSlider({ value, onChange, maxValue = DEFAULT_MAX }: LeverageSliderProps) {
  const trackRef = useRef<HTMLDivElement>(null);
  const thumbX = useMotionValue(0);
  const dragging = useRef(false);
  const [showHandle, setShowHandle] = useState(false);
  const reducedMotion = useReducedMotion();

  const totalSteps = DEFAULT_MAX - MIN_LEV;
  const trackWidth = () => trackRef.current?.clientWidth ?? 0;
  const leverageToX = (lev: number) => {
    const width = trackWidth();
    if (width === 0) return 0;
    return ((lev - MIN_LEV) / totalSteps) * width;
  };
  const xToLeverage = (x: number) => {
    const width = trackWidth();
    if (width === 0) return MIN_LEV;
    const clampedFraction = Math.max(0, Math.min(1, x / width));
    return Math.min(maxValue, Math.round(clampedFraction * totalSteps) + MIN_LEV);
  };

  useEffect(() => {
    if (!dragging.current) thumbX.set(leverageToX(value));
  });

  const springTo = useCallback(
    (lev: number) => {
      const clamped = Math.min(maxValue, lev);
      onChange(clamped);
      const target = leverageToX(clamped);
      if (reducedMotion) {
        thumbX.jump(target);
      } else {
        animate(thumbX, target, { type: 'spring', stiffness: 600, damping: 38, mass: 0.4 });
      }
    },
    [onChange, maxValue, reducedMotion], // eslint-disable-line react-hooks/exhaustive-deps
  );

  const handleThumbDown = (event: React.PointerEvent) => {
    event.stopPropagation();
    dragging.current = true;
    setShowHandle(true);
    const origin = { clientX: event.clientX, x: thumbX.get() };
    const maxX = leverageToX(maxValue);
    // onChange triggers a parent re-render, which recomputes every bar/tick
    // in this ramp (~100 elements) — firing it on every single pointermove
    // pixel-delta, most of which map to the same rounded step, was what
    // made dragging feel stuttery. thumbX (a motion value) still updates
    // every frame for a smooth thumb; onChange only fires when the actual
    // leverage step changes.
    let lastEmitted = value;

    const onMove = (moveEvent: PointerEvent) => {
      const nextX = Math.max(0, Math.min(maxX, origin.x + moveEvent.clientX - origin.clientX));
      thumbX.set(nextX);
      const nextLeverage = xToLeverage(nextX);
      if (nextLeverage !== lastEmitted) {
        lastEmitted = nextLeverage;
        onChange(nextLeverage);
      }
    };
    const onUp = (upEvent: PointerEvent) => {
      dragging.current = false;
      setShowHandle(false);
      const nextX = Math.max(0, Math.min(maxX, origin.x + upEvent.clientX - origin.clientX));
      springTo(xToLeverage(nextX));
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  };

  const handleKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === 'ArrowRight' || event.key === 'ArrowUp') {
      event.preventDefault();
      springTo(Math.min(maxValue, value + 1));
    } else if (event.key === 'ArrowLeft' || event.key === 'ArrowDown') {
      event.preventDefault();
      springTo(Math.max(MIN_LEV, value - 1));
    } else if (event.key === 'Home') {
      event.preventDefault();
      springTo(MIN_LEV);
    } else if (event.key === 'End') {
      event.preventDefault();
      springTo(maxValue);
    }
  };

  const majors = BAR_MARKS.map((mark) => ({
    pos: (mark - MIN_LEV) / totalSteps,
    height: barHeight(mark - MIN_LEV, totalSteps),
    active: mark <= value,
  }));

  const minors: { pos: number; height: number }[] = [];
  for (let i = 0; i < BAR_MARKS.length - 1; i++) {
    const stepA = (BAR_MARKS[i] ?? MIN_LEV) - MIN_LEV;
    const stepB = (BAR_MARKS[i + 1] ?? DEFAULT_MAX) - MIN_LEV;
    for (let j = 1; j <= 3; j++) {
      const fraction = j / 4;
      const step = stepA + fraction * (stepB - stepA);
      minors.push({
        pos: step / totalSteps,
        height: barHeight(stepA, totalSteps) + fraction * (barHeight(stepB, totalSteps) - barHeight(stepA, totalSteps)),
      });
    }
  }

  return (
    <MotionConfig reducedMotion="user">
    <div className="leverage-slider">
      <div className="leverage-slider__header">
        <p className="leverage-slider__label">Leverage</p>
        <motion.span
          key={value}
          initial={{ opacity: 0, y: -4 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ type: 'spring', stiffness: 500, damping: 30 }}
          className="leverage-slider__value"
        >
          {value}
        </motion.span>
        <span className="leverage-slider__unit">x</span>
      </div>

      <div
        ref={trackRef}
        className="leverage-slider__track"
        role="slider"
        tabIndex={0}
        aria-label="Leverage"
        aria-valuemin={MIN_LEV}
        aria-valuemax={maxValue}
        aria-valuenow={value}
        onKeyDown={handleKeyDown}
        onPointerEnter={() => setShowHandle(true)}
        onPointerLeave={() => {
          if (!dragging.current) setShowHandle(false);
        }}
        onClick={(event) => {
          const rect = trackRef.current!.getBoundingClientRect();
          springTo(Math.min(maxValue, xToLeverage(event.clientX - rect.left)));
        }}
      >
        <div className="leverage-slider__bars">
          {majors.map(({ pos, height, active }, i) => (
            <div
              key={`major-bar-${i}`}
              className="leverage-slider__bar leverage-slider__bar--major"
              data-active={active}
              style={{ left: `${pos * 100}%`, height }}
            />
          ))}
          {minors.map(({ pos, height }, i) => (
            <div
              key={`minor-bar-${i}`}
              className="leverage-slider__bar leverage-slider__bar--minor"
              style={{ left: `${pos * 100}%`, height }}
            />
          ))}
          {majors.map(({ pos, active }, i) => (
            <div
              key={`major-tick-${i}`}
              className="leverage-slider__tick leverage-slider__tick--major"
              data-active={active}
              style={{ left: `${pos * 100}%` }}
            />
          ))}
          {minors.map(({ pos }, i) => (
            <div key={`minor-tick-${i}`} className="leverage-slider__tick leverage-slider__tick--minor" style={{ left: `${pos * 100}%` }} />
          ))}

          <motion.div className="leverage-slider__thumb-hit" onPointerDown={handleThumbDown} style={{ x: thumbX }}>
            <div className="leverage-slider__thumb-line" />
          </motion.div>

          <AnimatePresence>
            {showHandle && (
              <motion.div
                className="leverage-slider__handle"
                initial={{ opacity: 0, scale: 0.7, y: 6 }}
                animate={{ opacity: 1, scale: 1, y: 0 }}
                exit={{ opacity: 0, scale: 0.7, y: 6 }}
                transition={{ type: 'spring', stiffness: 500, damping: 28, mass: 0.4 }}
                style={{ x: thumbX }}
              >
                <div className="leverage-slider__handle-grip">
                  {Array.from({ length: 6 }).map((_, i) => (
                    <div key={i} className="leverage-slider__handle-dot" />
                  ))}
                </div>
              </motion.div>
            )}
          </AnimatePresence>
        </div>

        <div className="leverage-slider__step-labels">
          {STEP_LABELS.map((lev) => (
            <button
              key={lev}
              type="button"
              className="leverage-slider__step-label"
              data-current={lev === value}
              disabled={lev > maxValue}
              style={{ left: `${((lev - MIN_LEV) / totalSteps) * 100}%` }}
              onClick={(event) => {
                event.stopPropagation();
                springTo(Math.min(maxValue, lev));
              }}
            >
              {lev}x
            </button>
          ))}
        </div>
      </div>
    </div>
    </MotionConfig>
  );
}
