import { useCallback, useEffect, useRef, useState } from 'react';
import { AnimatePresence, MotionConfig, animate, motion, useMotionValue, useReducedMotion } from 'framer-motion';

// Ported from cer-perp's trading panel (see app/design.md's "why this
// palette" note): a bar-chart ramp, not a flat <input type="range">, per
// design.md's Leverage control anti-patterns.
//
// Bar generation is derived entirely from the `maxValue` prop (TradePanel
// passes the real, backend-enforced ceiling: `10_000 / IMR_BPS`, 20x),
// not a hardcoded range: an earlier version baked in a fixed 1-50 ramp
// regardless of what maxValue was actually passed, so a 20x ceiling left
// most of the track rendering dead, unreachable ticks up to 50 and
// squeezed the real 1-20 range into a fraction of the width, sparse and
// choppy. One bar per whole step, plus 3 interpolated (decorative, not
// separately selectable) bars between each pair of steps, keeps the ramp
// dense regardless of how big maxValue is.

const MIN_LEV = 1;

function barHeight(step: number, totalSteps: number) {
  return 10 + (step / totalSteps) * 54;
}

export interface LeverageSliderProps {
  value: number;
  onChange: (value: number) => void;
  maxValue?: number;
}

export function LeverageSlider({ value, onChange, maxValue = 20 }: LeverageSliderProps) {
  const trackRef = useRef<HTMLDivElement>(null);
  const thumbX = useMotionValue(0);
  const dragging = useRef(false);
  const [showHandle, setShowHandle] = useState(false);
  const reducedMotion = useReducedMotion();

  const totalSteps = maxValue - MIN_LEV;
  const barMarks = Array.from({ length: totalSteps + 1 }, (_, i) => MIN_LEV + i);
  // Clickable text row underneath: a trade panel this narrow can't fit a
  // label per whole step without them colliding, so this is a small,
  // evenly-spaced subset of barMarks — every other step is still a real,
  // reachable value (drag or arrow keys), it just isn't its own button.
  const labelCount = Math.min(6, totalSteps + 1);
  const stepLabels = Array.from(
    new Set(
      Array.from({ length: labelCount }, (_, i) =>
        Math.round(MIN_LEV + (i / (labelCount - 1)) * totalSteps),
      ),
    ),
  );
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

  const majors = barMarks.map((mark) => ({
    pos: (mark - MIN_LEV) / totalSteps,
    height: barHeight(mark - MIN_LEV, totalSteps),
    active: mark <= value,
  }));

  const minors: { pos: number; height: number }[] = [];
  for (let i = 0; i < barMarks.length - 1; i++) {
    const stepA = (barMarks[i] ?? MIN_LEV) - MIN_LEV;
    const stepB = (barMarks[i + 1] ?? maxValue) - MIN_LEV;
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
          {stepLabels.map((lev) => (
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
