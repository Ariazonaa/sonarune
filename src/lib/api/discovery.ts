import { invoke } from "@tauri-apps/api/core";
import type { AlbumDto, ArtistDto, GenreDto, Page, TrackDto } from "$lib/types";

export interface DiscoverRow<T> {
  items: T[];
  error: string | null;
}

export interface SimilarArtistsRow extends DiscoverRow<ArtistDto> {
  seed: ArtistDto | null;
}

export interface DecadeDto {
  startYear: number;
  label: string;
}

export interface DiscoverData {
  randomAlbums: DiscoverRow<AlbumDto>;
  genres: DiscoverRow<GenreDto>;
  instantMixSeeds: DiscoverRow<TrackDto>;
  similarArtists: SimilarArtistsRow;
  decades: DiscoverRow<DecadeDto>;
  longNotHeard: DiscoverRow<AlbumDto>;
}

export type SmartPlayed = "all" | "played" | "unplayed";

export interface SmartFilter {
  yearFrom?: number | null;
  yearTo?: number | null;
  genreIds: string[];
  played: SmartPlayed;
  favoriteOnly: boolean;
  minPlayCount?: number | null;
  /** ISO calendar date (YYYY-MM-DD), interpreted by the server. */
  addedSince?: string | null;
}

export interface CollectionDto {
  id: string;
  name: string;
  albumCount: number;
  imageTag?: string | null;
  imageBlurHash?: string | null;
}

export interface CollectionDetail {
  collection: CollectionDto;
  albums: AlbumDto[];
}

export const apiDiscovery = {
  getDiscover: () => invoke<DiscoverData>("get_discover"),
  getRandomAlbums: (startIndex: number, limit: number) =>
    invoke<Page<AlbumDto>>("get_discover_random_albums", { startIndex, limit }),
  getMixSeeds: (startIndex: number, limit: number) =>
    invoke<Page<TrackDto>>("get_discover_mix_seeds", { startIndex, limit }),
  getSimilarArtists: (artistId: string | null, limit: number) =>
    invoke<SimilarArtistsRow>("get_discover_similar_artists", { artistId, limit }),
  getDecades: () => invoke<DecadeDto[]>("get_discover_decades"),
  getDecadeAlbums: (startYear: number, startIndex: number, limit: number) =>
    invoke<Page<AlbumDto>>("get_discover_decade_albums", { startYear, startIndex, limit }),
  getLongNotHeard: (startIndex: number, limit: number) =>
    invoke<Page<AlbumDto>>("get_long_not_heard_albums", { startIndex, limit }),
  playAlbums: (albumIds: string[], shuffle: boolean) =>
    invoke<void>("play_discover_albums", { albumIds, shuffle }),
  playTracks: (trackIds: string[], shuffle: boolean) =>
    invoke<void>("play_discover_tracks", { trackIds, shuffle }),
  playGenre: (genreId: string, shuffle: boolean) =>
    invoke<void>("play_discover_genre", { genreId, shuffle }),
  playDecade: (startYear: number, shuffle: boolean) =>
    invoke<void>("play_discover_decade", { startYear, shuffle }),

  querySmartView: (filter: SmartFilter, startIndex: number, limit: number) =>
    invoke<Page<TrackDto>>("query_smart_view", { filter, startIndex, limit }),
  playSmartView: (filter: SmartFilter, shuffle: boolean) =>
    invoke<void>("play_smart_view", { filter, shuffle }),
  materializeSmartView: (name: string, filter: SmartFilter) =>
    invoke<string>("materialize_smart_view", { name, filter }),

  getCollections: (startIndex: number, limit: number) =>
    invoke<Page<CollectionDto>>("get_collections", { startIndex, limit }),
  getCollection: (collectionId: string) =>
    invoke<CollectionDetail>("get_collection", { collectionId }),
  playCollection: (collectionId: string, shuffle: boolean) =>
    invoke<void>("play_collection", { collectionId, shuffle }),
  enqueueCollection: (collectionId: string, next: boolean, shuffle = false) =>
    invoke<void>("enqueue_collection", { collectionId, next, shuffle }),
};
