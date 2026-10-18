# File Browser Server — planteamiento

Proyecto **aparte** dentro del mismo repo que `filebrowser`, en `server/`: crate propio
(*workspace member*) con su `Cargo.toml` y binario `filebrowser-server`. Los ejecutables clásicos
no se tocan en `src/`, no ganan dependencias de red y no cambian de tamaño.
El `Cargo.toml` raíz solo recibe `[workspace] members = ["server"]` y `default-members = ["."]`,
de modo que `cargo build --release` sigue compilando solo el gestor y el servidor se construye con
`cargo build --release -p filebrowser-server`.

## Objetivo
Servir carpetas del PC por HTTP para verlas y descargarlas desde un navegador (móvil u otro PC),
con el menor tamaño y la menor superficie de ataque posibles. Sin cuentas, sin base de datos,
sin panel de administración.

## No objetivos (v1)
- Subir, borrar, renombrar o editar archivos (solo lectura).
- Usuarios y permisos múltiples.
- Exposición directa a internet (se recomienda túnel: Tailscale, WireGuard, Cloudflare Tunnel).
- Streaming con transcodificación, WebDAV, SMB.

## Uso
```
filebrowser-server ~/Documentos ~/Fotos        # comparte esas carpetas
filebrowser-server --port 8080 --bind 0.0.0.0  # por defecto: 127.0.0.1 y puerto aleatorio
filebrowser-server --ttl 2h --read-only        # caduca; solo lectura (único modo en v1)
```
Al arrancar imprime una URL con token: `http://192.168.1.20:41873/s/<token>/` y un QR en terminal.
Ctrl+C la invalida. El token no se guarda en disco.

## Seguridad (lo central)
1. **Token aleatorio** de 128 bits (de `getrandom`) en la ruta; sin token válido, 404 igual que una ruta inexistente.
   Comparación en tiempo constante. Limitador de intentos fallidos por IP.
2. **Confinamiento**: cada raíz se canonicaliza al inicio. Cada petición: decodificar % una sola vez,
   rechazar `..`, `\`, NUL, `:` en Windows, y tras unir comprobar que el `canonicalize` sigue
   dentro de la raíz (los enlaces simbólicos que salen se omiten). Apertura sin seguir el último
   componente simbólico cuando se pueda (`O_NOFOLLOW`).
3. **Solo ficheros regulares y directorios**: nada de dispositivos, FIFOs ni sockets.
4. **Límites**: máx. conexiones simultáneas, cabeceras ≤ 8 KiB, timeout de lectura/escritura,
   máx. entradas por listado, sin `Transfer-Encoding` raro, solo GET/HEAD.
5. **Cabeceras**: `Content-Security-Policy` estricta, `X-Content-Type-Options: nosniff`,
   `Content-Disposition: attachment` por defecto (previsualización inline solo de imágenes/vídeo/PDF
   con tipos MIME de una lista cerrada; SVG y HTML siempre como descarga).
6. **Escape de HTML** en nombres de archivo en el listado; nombres con saltos de línea o control, saneados.
7. **Red**: por defecto solo `127.0.0.1`. `--bind` explícito para LAN, con aviso de que no hay HTTPS.
   Opción posterior: HTTPS con certificado propio generado (rustls) y huella impresa en consola.
8. **Sin ejecución**: ningún endpoint lanza procesos. Miniaturas solo con decodificadores acotados
   (los mismos topes de píxeles/memoria ya auditados en el gestor).
9. **Privilegios**: se niega a arrancar como root/Administrador salvo `--i-know`.

## Funciones v1
- Listado de directorio (HTML sin JS obligatorio, ordenable, responsive).
- Descarga con `Range` (reanudar y saltar en vídeo/audio), `ETag`/`Last-Modified`.
- Descarga de carpeta como ZIP en streaming (sin buffer completo, con tope opcional).
- Previsualización inline: imágenes, vídeo, audio, PDF, texto.
- Miniaturas bajo demanda con caché en memoria acotada (fase 2).
- Búsqueda por nombre dentro de la raíz (fase 2).

## Arquitectura
- **Crate** `server/`, binario `filebrowser-server`. Servidor HTTP/1.1 síncrono con pool de hilos fijo
  (`tiny_http` o implementación mínima propia sobre `std::net`); sin tokio para mantener el tamaño.
- Módulos: `main` (CLI), `server` (aceptación, límites), `route` (token + rutas), `jail`
  (confinamiento, la parte más revisada), `index` (HTML del listado), `range`, `zip_stream`, `mime`.
- **Reutilización del gestor**: M0 no depende de la `lib` (que arrastra egui). Para miniaturas
  (M2) se extraen las piezas puras (decodificación acotada) a un crate pequeño compartido, o se
  usa la `lib` tras un *feature* sin GUI. Nunca al revés: `filebrowser` no depende de `filebrowser-server`.
- Registro mínimo por stderr (sin rutas completas del sistema de archivos en las respuestas de error).

## Integración opcional con el gestor (sin tocar su binario)
- Fase 3: una entrada de menú «Compartir esta carpeta…» que simplemente **lanza** `filebrowser-server`
  si está instalado (como cualquier otro programa externo), mostrando la URL. Si no está instalado,
  la entrada no aparece. El gestor no enlaza ningún código de red.

## Distribución
- Artefacto separado: `filebrowser-server-{linux,windows,macos}` (perfil de release propio del crate); objetivo < 2 MB (`opt-level="z"`, LTO, strip).
- Pruebas: unitarias de `jail` (traversal, simbólicos, codificaciones dobles, Unicode, Windows `\\?\`),
  fuzz de parseo de petición y de rutas, prueba de integración con `curl`.

## Estado y hitos
- **M0 (hecho)** crate `server/`, `jail` con tests (traversal, `%2e%2e`, doble codificación, enlaces que salen o apuntan a ocultos,
  prefijo parecido), token, listado, descarga con `Range`/`ETag`/304, solo `127.0.0.1` por defecto.
- **M1 (hecho)** `--bind` para LAN, límite de conexiones (503) global y por IP, tiempos de lectura (408) y escritura, cabeceras acotadas (431),
  bloqueo por IP tras 20 tokens falsos en un minuto (429), ZIP en streaming (stored + ZIP64, validado con `unzip` y la crate `zip`),
  QR en terminal, `--ttl`, negativa a ejecutarse como root, dotfiles ocultos por defecto.
- **M2 (hecho)** previsualización inline de lista cerrada (imágenes, vídeo, audio, PDF, texto), búsqueda por nombre
  (tope de 300 resultados, 3 s y 200 000 entradas) y vista previa perezosa de imágenes ≤ 1 MiB en el listado.
  Sin miniaturas generadas en el servidor: decodificar imágenes ajenas aquí añadiría superficie de ataque; las decodifica el navegador.
- **M3 (hecho, salvo lo indicado)** `--tls`: HTTPS con certificado autofirmado de 30 días generado al arrancar (rustls + rcgen, característica
  `tls` activa por defecto; `--no-default-features` da un binario de ~0,6 MB sin TLS) y huella SHA-256 impresa.
  **No hecho a propósito:** entrada «Compartir» en el gestor (rompe la regla de no tocar el gestor; el servidor se lanza desde la terminal)
  y auditoría de seguridad dedicada con agentes (pendiente de que se pida).

## Decisiones abiertas
- ~~`tiny_http` o servidor propio~~: resuelto, servidor propio sobre `std::net` (sin dependencias de red, ~400 líneas de HTTP/enrutado a auditar).
- ¿Android? Un servidor en el móvil implica servicio en primer plano; se deja fuera de v1.
- ¿Subida de archivos más adelante? Si sí, será otro modo explícito con su propia auditoría.
