"use client";

import { useEffect, useRef } from "react";

// Fixed full-viewport canvas: twinkling starfield + occasional shooting star.
// Pure decoration (aria-hidden). Static export safe: everything runs in
// useEffect, renders nothing on the server. Honors prefers-reduced-motion
// by painting one static frame.
export default function Starfield({ density = 170 }) {
  const ref = useRef(null);

  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const reduced = window.matchMedia(
      "(prefers-reduced-motion: reduce)"
    ).matches;

    let w = 0;
    let h = 0;
    let stars = [];
    let raf = 0;
    let meteor = null;
    let nextMeteor = performance.now() + 4000 + Math.random() * 6000;

    const seed = () => {
      const n = Math.min(
        density,
        Math.floor((window.innerWidth * window.innerHeight) / 9000)
      );
      stars = Array.from({ length: n }, () => ({
        x: Math.random() * w,
        y: Math.random() * h,
        r: 0.4 + Math.random() * 1.2,
        a: 0.25 + Math.random() * 0.6,
        speed: 0.4 + Math.random() * 1.4,
        phase: Math.random() * Math.PI * 2,
        tint: Math.random(),
      }));
    };

    const resize = () => {
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      w = window.innerWidth;
      h = window.innerHeight;
      canvas.width = Math.floor(w * dpr);
      canvas.height = Math.floor(h * dpr);
      canvas.style.width = `${w}px`;
      canvas.style.height = `${h}px`;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      seed();
    };

    const drawStars = (t) => {
      ctx.clearRect(0, 0, w, h);
      for (const s of stars) {
        const tw = reduced ? 1 : 0.55 + 0.45 * Math.sin(s.phase + t * 0.001 * s.speed);
        const alpha = Math.max(0.05, s.a * tw);
        ctx.globalAlpha = alpha;
        ctx.fillStyle =
          s.tint > 0.86 ? "#9fd4ff" : s.tint < 0.12 ? "#ffe9c4" : "#ffffff";
        ctx.beginPath();
        ctx.arc(s.x, s.y, s.r, 0, Math.PI * 2);
        ctx.fill();
      }
      ctx.globalAlpha = 1;
    };

    const drawMeteor = (now, dt) => {
      if (!meteor && !reduced && now >= nextMeteor) {
        const fromLeft = Math.random() > 0.5;
        meteor = {
          x: fromLeft ? -60 : w * (0.3 + Math.random() * 0.7),
          y: Math.random() * h * 0.4,
          vx: (fromLeft ? 1 : -0.4) * (9 + Math.random() * 5),
          vy: 3 + Math.random() * 2,
          life: 1,
        };
      }
      if (!meteor) return;
      meteor.x += meteor.vx * dt * 60 * 0.016;
      meteor.y += meteor.vy * dt * 60 * 0.016;
      meteor.life -= dt * 1.4;
      if (meteor.life <= 0 || meteor.x > w + 120 || meteor.y > h + 120) {
        meteor = null;
        nextMeteor = now + 5000 + Math.random() * 9000;
        return;
      }
      const grad = ctx.createLinearGradient(
        meteor.x,
        meteor.y,
        meteor.x - meteor.vx * 12,
        meteor.y - meteor.vy * 12
      );
      grad.addColorStop(0, `rgba(255,255,255,${0.9 * meteor.life})`);
      grad.addColorStop(1, "rgba(120,180,255,0)");
      ctx.strokeStyle = grad;
      ctx.lineWidth = 1.6;
      ctx.beginPath();
      ctx.moveTo(meteor.x, meteor.y);
      ctx.lineTo(meteor.x - meteor.vx * 12, meteor.y - meteor.vy * 12);
      ctx.stroke();
    };

    let last = performance.now();
    let frame = null;
    if (!reduced) {
      frame = (now) => {
        const dt = Math.min(0.05, (now - last) / 1000);
        last = now;
        drawStars(now);
        drawMeteor(now, dt);
        raf = requestAnimationFrame(frame);
      };
      resize();
      raf = requestAnimationFrame(frame);
    } else {
      resize();
      drawStars(0);
    }

    window.addEventListener("resize", resize);
    const onVis = () => {
      if (document.hidden) cancelAnimationFrame(raf);
      else if (!reduced && frame) {
        last = performance.now();
        raf = requestAnimationFrame(frame);
      }
    };
    document.addEventListener("visibilitychange", onVis);
    return () => {
      cancelAnimationFrame(raf);
      window.removeEventListener("resize", resize);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [density]);

  return <canvas ref={ref} className="starfield" aria-hidden="true" />;
}
