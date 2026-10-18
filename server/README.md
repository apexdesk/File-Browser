# filebrowser-server

Comparte carpetas del PC por HTTP, **solo lectura**, con un enlace secreto. Proyecto aparte del
gestor de archivos: es otro binario y el gestor no lleva nada de este código de red.

```
cargo build --release -p filebrowser-server       # desde la raíz del repo (con HTTPS, ~1,9 MB)
cargo build --release -p filebrowser-server --no-default-features   # sin TLS, ~0,6 MB
target/release/filebrowser-server ~/Documentos ~/Fotos
target/release/filebrowser-server --bind 0.0.0.0 --ttl 2h ~/Descargas   # red local, caduca en 2 h
target/release/filebrowser-server --bind 0.0.0.0 --tls ~/Descargas      # con HTTPS autofirmado (compara la huella)
```

Imprime un enlace como `http://192.168.1.20:41873/s/<token>/` y un QR. El token (128 bits) solo existe
en memoria mientras el programa corre; sin él, todo responde 404.

## Qué hace
- Listado de carpetas (HTML sin JavaScript), orden por nombre/tamaño/fecha, búsqueda por nombre.
- Descarga con `Range` (saltar en vídeo/audio, reanudar), `ETag` y 304.
- Descarga de una carpeta como ZIP en streaming (sin comprimir, ZIP64).
- Vista previa pequeña de imágenes ≤ 1 MiB en el listado (la decodifica el navegador).
- Muestra en el navegador solo imágenes, vídeo, audio, PDF y texto; HTML, SVG y demás se descargan siempre.

## Qué no hace
- No sube, borra ni modifica nada. No hay cuentas. HTTPS solo con `--tls` y certificado autofirmado
  (el navegador avisa; compara la huella SHA-256 impresa en la terminal).
- Para salir a internet, usa un túnel (WireGuard, Tailscale, Cloudflare Tunnel); no abras el puerto del router.

## Seguridad
- Cada ruta se decodifica una vez, se valida por componentes y se canonicaliza; si el resultado sale de la
  carpeta compartida se responde 404 (enlaces simbólicos incluidos). Los archivos y carpetas que empiezan
  por `.` no se sirven salvo con `--hidden`.
- Solo archivos regulares y carpetas. Una petición por conexión, solo GET/HEAD, cabeceras ≤ 8 KiB,
  10 s para recibirlas, límite de conexiones simultáneas (global y 8 por IP) y de intentos fallidos por IP.
- Cabeceras `nosniff`, `Content-Security-Policy` restrictiva, `Referrer-Policy: no-referrer`.
- Se niega a ejecutarse como root/administrador sin `--i-know`.
- Limitación conocida: entre la comprobación de la ruta y la apertura del archivo hay una ventana
  mínima (TOCTOU) si alguien con acceso local cambia un enlace en ese instante.
