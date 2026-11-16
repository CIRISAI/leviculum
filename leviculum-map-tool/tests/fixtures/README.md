# leviculum-map-tool test fixtures

## `Copernicus_DSM_COG_10_N31_00_W004_00_DEM.head.tif`

The first 1,224 bytes of a real Copernicus DEM GLO-30 cell: the TIFF
header, the first IFD and every value it points to (pixel scale,
tiepoint, geokeys, the tile offset and count arrays), no height sample.
`tests/heights.rs` reads its georeference and checks it against the
cell's own metadata (`Copernicus_DSM_10_N31_00_W004_00.xml` beside it in
the bucket: upper left -4.00000, 32.00000, 1.0 arcsec spacing).

Fetched 2026-11-12:

```sh
N=Copernicus_DSM_COG_10_N31_00_W004_00_DEM
curl -r 0-1223 -o $N.head.tif https://copernicus-dem-30m.s3.amazonaws.com/$N/$N.tif
```

Produced using Copernicus WorldDEM-30 © DLR e.V. 2010-2014 and © Airbus
Defence and Space GmbH 2014-2018 provided under COPERNICUS by the
European Union and ESA; all rights reserved. Licence for the use of the
Copernicus WorldDEM-30 (`INFO/eula_F.pdf` beside every cell). The
organisations in charge of the Copernicus programme by law or by
delegation do not incur any liability for any use of the Copernicus
WorldDEM-30.
