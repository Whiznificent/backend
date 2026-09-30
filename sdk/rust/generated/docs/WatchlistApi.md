# \WatchlistApi

All URIs are relative to *http://localhost*

Method | HTTP request | Description
------------- | ------------- | -------------
[**add_watchlist_v1**](WatchlistApi.md#add_watchlist_v1) | **POST** /api/v1/watchlist | 
[**add_watchlist_v2**](WatchlistApi.md#add_watchlist_v2) | **POST** /api/v2/watchlist | 
[**get_watchlist_v1**](WatchlistApi.md#get_watchlist_v1) | **GET** /api/v1/watchlist | 
[**get_watchlist_v2**](WatchlistApi.md#get_watchlist_v2) | **GET** /api/v2/watchlist | 
[**remove_watchlist_v1**](WatchlistApi.md#remove_watchlist_v1) | **DELETE** /api/v1/watchlist/{underlying} | 
[**remove_watchlist_v2**](WatchlistApi.md#remove_watchlist_v2) | **DELETE** /api/v2/watchlist/{underlying} | 



## add_watchlist_v1

> std::collections::HashMap<String, serde_json::Value> add_watchlist_v1(watchlist_request)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**watchlist_request** | [**WatchlistRequest**](WatchlistRequest.md) |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

[bearerAuth](../README.md#bearerAuth)

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## add_watchlist_v2

> std::collections::HashMap<String, serde_json::Value> add_watchlist_v2(watchlist_request)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**watchlist_request** | [**WatchlistRequest**](WatchlistRequest.md) |  | [required] |

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

[bearerAuth](../README.md#bearerAuth)

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_watchlist_v1

> std::collections::HashMap<String, serde_json::Value> get_watchlist_v1()


### Parameters

This endpoint does not need any parameter.

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

[bearerAuth](../README.md#bearerAuth)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_watchlist_v2

> std::collections::HashMap<String, serde_json::Value> get_watchlist_v2()


### Parameters

This endpoint does not need any parameter.

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

[bearerAuth](../README.md#bearerAuth)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## remove_watchlist_v1

> remove_watchlist_v1(underlying)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**underlying** | **String** |  | [required] |

### Return type

 (empty response body)

### Authorization

[bearerAuth](../README.md#bearerAuth)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: Not defined

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## remove_watchlist_v2

> remove_watchlist_v2(underlying)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**underlying** | **String** |  | [required] |

### Return type

 (empty response body)

### Authorization

[bearerAuth](../README.md#bearerAuth)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: Not defined

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

