# Archivos

Gestor de archivos multiplataforma (Linux / Windows / macOS) en Rust, pensado para carpetas con
cientos de miles de elementos y miniaturas.

## Compilar y ejecutar

    cargo run --release [-- /ruta/inicial]

## Diseño de rendimiento

- **Listado en streaming** en un hilo: el primer bloque se pinta en ~1 ms; 300 000 entradas en ~0,25 s.
- **Orden/filtro sólo sobre índices `u32`** en un hilo aparte; la UI nunca se bloquea.
- **Cuadrícula/lista virtualizada**: sólo se dibujan las filas visibles (+1 de prefetch).
- **Miniaturas**: cola "lo visible primero" que se reemplaza cada frame (desplazarse rápido descarta
  trabajo obsoleto), pool de hilos, JPEG con escalado DCT (1/2–1/8), caché LRU de texturas.
- **Iconos vectoriales** dibujados con el painter: sin imágenes ni fuentes de iconos.
- Sin repintado continuo: CPU 0 % en reposo.

## Atajos

| Tecla | Acción |
|---|---|
| Espacio | Vista rápida (←/→ navegan, Esc/Espacio cierran, Enter abre) |
| Enter / doble clic | Abrir |
| Retroceso, Alt+↑ | Subir · Alt+←/→ atrás/adelante |
| F2 | Renombrar · Supr papelera · Mayús+Supr borrar |
| Ctrl+C/X/V | Copiar/cortar/pegar · Ctrl+A seleccionar todo |
| Ctrl+L | Editar ruta · Ctrl+F filtrar · Ctrl+D marcador |
| Ctrl+H | Ocultos · Ctrl+Mayús+N nueva carpeta · F5 actualizar |
| Teclear | Salta al primer nombre que empiece por lo escrito |

Arrastra elementos sobre una carpeta o sobre la barra lateral para moverlos; arrastra en vacío
para selección por lazo.

## Novedades

- **Menú** (Archivo / Editar / Ver / Ir / Herramientas / Ayuda) con las operaciones habituales: nuevo archivo, duplicar, renombrar en lote, propiedades (tamaño recursivo en segundo plano), comprimir/extraer ZIP, buscar en subcarpetas, opciones.
- **Pestañas**: `Ctrl+T` nueva, `Ctrl+W` cerrar, `Ctrl+Tab` / `Ctrl+Mayús+Tab` cambiar, clic central para cerrar.
- **Iconos del sistema** (tema de iconos freedesktop en Linux, Shell en Windows; en macOS se usan los vectoriales). Se desactivan en Opciones.
- **Terminal integrada** opcional en la parte inferior (`F12` o `Ctrl+\``): PTY real + emulación VT100.
- La sección **Marcadores** sólo aparece si hay marcadores.
- Miniaturas: rotación EXIF, SVG, vídeo/HEIC y otros formatos mediante `ffmpeg` o ImageMagick si están instalados.
- Fechas en hora local con segundos.
- **Soltar archivos** desde otra aplicación sobre la ventana los copia a la carpeta actual.
- **Portapapeles de archivos** compatible con otros gestores en Linux si hay `wl-copy`/`xclip` (`wl-paste`/`xclip` para pegar; con sólo archivos en el portapapeles hay que usar el menú «Pegar», porque Ctrl+V del sistema sólo llega con texto).

### Limitaciones conocidas

- **Arrastrar archivos desde la aplicación hacia otras apps** no está soportado (winit no ofrece origen de arrastre y soltar en Linux). Alternativa: copiar y pegar.
- En Windows/macOS el portapapeles de archivos con otras apps no está implementado (sólo texto con rutas).
- La compilación para Windows se ha comprobado con `cargo check`; no se ha ejecutado en Windows ni macOS.

## Novedades (v3)

- **Barra de título integrada** con botones de ventana propios (opción para usar la nativa).
- **Menú hamburguesa** (☰, F10) con todas las funciones en submenús; se puede ocultar desde el propio menú u Opciones.
- **Pestañas**: la barra solo aparece con 2 o más; Ctrl+T crea la nueva pestaña junto a la actual.
- **Reproductor de vídeo** integrado (requiere `ffmpeg` y `ffplay` en el PATH): Espacio/Enter reproduce o pausa, ←/→ salta 5 s, barra de búsqueda.
- **Idiomas**: español, inglés, francés, alemán, portugués, italiano, ruso, chino, japonés y coreano; «Usar el idioma del sistema» en Opciones. Árabe e hindi no se incluyen porque egui no soporta aún RTL/shaping. CJK usa una fuente del sistema.

## Novedades (v4)

- **Ventana con esquinas redondeadas** (transparente; se desactiva al maximizar o con barra nativa).
- **`.hidden`**: los nombres listados en un archivo `.hidden` de la carpeta se ocultan.
- **Deshacer / rehacer** (Ctrl+Z / Ctrl+Y): renombrar, mover, copiar, crear, papelera, ZIP.
- **Cola de operaciones** con botón Cancelar y **diálogo de conflictos** (reemplazar / mantener ambos / omitir; lo reemplazado va a la papelera).
- **Abrir con…**, **papelera navegable** (Ir → Papelera; Linux y Windows), **hash** MD5/SHA-1/SHA-256, **buscador de duplicados**.
- **Autocompletado de ruta** (Tab, ↑/↓, clic), **clic central** abre carpeta en pestaña nueva, **Ctrl+Mayús+T** reabre pestaña, **restaurar pestañas** al iniciar, **Nueva ventana** (Ctrl+N).
- Vista dividida (F9): dos paneles, lado a lado o apilados; cada pestaña guarda su vista completa (una o dos carpetas) y su título suma ambos nombres («A | B»).
- Paleta de comandos (Mayús+Espacio) y atajos reasignables.
- Ajustes de vista (rueda dentada, a la derecha): ordenar, mostrar ocultos y tema.

- **Visor en ventana aparte** (opcional): Opciones → "Abrir el visor en una ventana aparte". La ventana es independiente, redimensionable y admite las mismas teclas (Espacio/Esc, flechas, Enter, O).
