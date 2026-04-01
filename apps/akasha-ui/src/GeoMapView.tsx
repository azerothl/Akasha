import L from "leaflet";
import "leaflet/dist/leaflet.css";
import { useEffect, useRef } from "react";

/** Points use x = longitude, y = latitude (same as GeoJSON / map plugin). */
export type GeoMapPoint = { x: number; y: number };

export type GeoMapViewProps = {
  points: GeoMapPoint[];
  /** CSS min-height in pixels (default 260). */
  height?: number;
  className?: string;
  /** Accessible name for the map region. */
  ariaLabel?: string;
};

/**
 * Interactive map (Leaflet + OSM tiles) — generic: any tool payload with lon/lat polyline.
 */
export function GeoMapView({ points, height = 260, className, ariaLabel = "Interactive map" }: GeoMapViewProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const mapRef = useRef<L.Map | null>(null);

  useEffect(() => {
    const el = containerRef.current;
    if (!el || points.length < 2) return;

    const map = L.map(el, { scrollWheelZoom: true });
    L.tileLayer("https://tile.openstreetmap.org/{z}/{x}/{y}.png", {
      attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>',
      maxZoom: 19,
    }).addTo(map);

    const latlngs = points.map((p) => L.latLng(p.y, p.x));
    L.polyline(latlngs, { color: "#0ea5e9", weight: 4, opacity: 0.92 }).addTo(map);
    if (points.length >= 2) {
      L.circleMarker(latlngs[0]!, { radius: 5, color: "#0369a1", fillColor: "#38bdf8", fillOpacity: 0.9 }).addTo(map);
      L.circleMarker(latlngs[latlngs.length - 1]!, {
        radius: 5,
        color: "#0369a1",
        fillColor: "#38bdf8",
        fillOpacity: 0.9,
      }).addTo(map);
    }
    map.fitBounds(L.latLngBounds(latlngs), { padding: [18, 18] });
    mapRef.current = map;

    const fixLayout = () => {
      map.invalidateSize();
    };
    map.whenReady(() => {
      requestAnimationFrame(fixLayout);
      window.setTimeout(fixLayout, 120);
    });

    return () => {
      map.remove();
      mapRef.current = null;
    };
  }, [points]);

  if (points.length < 2) return null;

  return (
    <div
      ref={containerRef}
      className={className ?? "geo-map-view"}
      style={{ minHeight: height, width: "100%", borderRadius: 10 }}
      role="region"
      aria-label={ariaLabel}
      tabIndex={0}
    />
  );
}
